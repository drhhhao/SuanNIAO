use std::{
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};

use rustls::ServerConnection;

const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// An authenticated TLS stream. Clones share one connection, not independent
/// read cursors. Use one reader and any number of writers. Each `write_all`
/// stays contiguous relative to other writes, including writes larger than a
/// TLS record. TCP reads and writes never hold the TLS state mutex.
#[derive(Clone)]
pub struct SecureConn {
    inner: Arc<Inner>,
}

struct Inner {
    tls: Mutex<ServerConnection>,
    // The reader owns pending ciphertext across calls. Never feed more of it
    // to rustls until the application has drained available plaintext.
    read_gate: Mutex<PendingInput>,
    write_gate: WriteGate,
    reader: TcpStream,
    writer: TcpStream,
    closed: AtomicBool,
}

struct PendingInput {
    bytes: [u8; 16 * 1024],
    start: usize,
    end: usize,
}

#[derive(Default)]
struct WriteGate {
    busy: Mutex<bool>,
    ready: Condvar,
}

struct WriteGuard<'a>(&'a WriteGate);

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut busy) = self.0.busy.lock() {
            *busy = false;
        }
        self.0.ready.notify_one();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> io::Result<MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| io::Error::other("SuanNIAO TLS state lock poisoned"))
}

impl SecureConn {
    pub(crate) fn new(tcp: TcpStream, tls: ServerConnection) -> io::Result<Self> {
        #[cfg(windows)]
        configure_keepalive(&tcp)?;
        tcp.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let writer = tcp.try_clone()?;
        Ok(Self {
            inner: Arc::new(Inner {
                tls: Mutex::new(tls),
                read_gate: Mutex::new(PendingInput {
                    bytes: [0; 16 * 1024],
                    start: 0,
                    end: 0,
                }),
                write_gate: WriteGate::default(),
                reader: tcp,
                writer,
                closed: AtomicBool::new(false),
            }),
        })
    }

    /// Read plaintext. A clean TLS close returns zero; a truncated TLS stream
    /// reports `UnexpectedEof`. Local `shutdown` also wakes a pending read.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut pending = lock(&self.inner.read_gate)?;
        let result = self.read_inner(buf, &mut pending);
        if result.is_err() {
            self.abort();
        }
        result
    }

    fn read_inner(&self, buf: &mut [u8], pending: &mut PendingInput) -> io::Result<usize> {
        loop {
            if self.inner.closed.load(Ordering::Acquire) {
                return Ok(0);
            }
            {
                let mut tls = lock(&self.inner.tls)?;
                match tls.reader().read(buf) {
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                    result => return result,
                }
                if pending.start < pending.end {
                    let count = tls.read_tls(&mut &pending.bytes[pending.start..pending.end])?;
                    if count == 0 {
                        return Err(io::Error::other("TLS input made no progress"));
                    }
                    pending.start += count;
                    tls.process_new_packets()
                        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                    drop(tls);
                    // Key-update replies use the same serialized output path
                    // as application data, without holding the TLS mutex.
                    self.flush_pending()?;
                    // Always drain plaintext (or observe close_notify) before
                    // feeding the next record, even within one TCP read.
                    continue;
                }
            }

            // This is the potentially unbounded idle wait. In particular, do
            // not hold tls or write_gate while waiting for encrypted bytes.
            let mut reader = &self.inner.reader;
            let count = match reader.read(&mut pending.bytes) {
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) if self.inner.closed.load(Ordering::Acquire) => return Ok(0),
                result => result?,
            };
            pending.start = 0;
            pending.end = count;
            if count == 0 {
                // Teach rustls about EOF, so a missing close_notify is reported.
                lock(&self.inner.tls)?.read_tls(&mut io::empty())?;
            }
        }
    }

    /// Write all bytes as one non-interleaved application write within five
    /// seconds, including waiting for other writers. Any error closes the
    /// stream and wakes readers: partially emitted TLS records cannot be retried.
    pub fn write_all(&self, data: &[u8]) -> io::Result<()> {
        let deadline = Instant::now() + WRITE_TIMEOUT;
        let _writer = match self.lock_writer(deadline) {
            Ok(writer) => writer,
            Err(error) => {
                self.abort();
                return Err(error);
            }
        };
        let result = self.write_inner(data, deadline);
        if result.is_err() {
            self.abort();
        }
        result
    }

    fn write_inner(&self, mut data: &[u8], deadline: Instant) -> io::Result<()> {
        self.ensure_open()?;
        while !data.is_empty() {
            remaining_write_time(deadline)?;
            let (count, encrypted) = {
                let mut tls = lock(&self.inner.tls)?;
                let count = tls.writer().write(&data[..data.len().min(16 * 1024)])?;
                let encrypted = take_tls_output(&mut tls)?;
                (count, encrypted)
            };
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "TLS write made no progress",
                ));
            }
            self.write_encrypted(&encrypted, deadline)?;
            data = &data[count..];
        }
        remaining_write_time(deadline).map(|_| ())
    }

    fn lock_writer(&self, deadline: Instant) -> io::Result<WriteGuard<'_>> {
        let gate = &self.inner.write_gate;
        let mut busy = lock(&gate.busy)?;
        loop {
            self.ensure_open()?;
            let remaining = remaining_write_time(deadline)?;
            if !*busy {
                *busy = true;
                return Ok(WriteGuard(gate));
            }
            (busy, _) = gate
                .ready
                .wait_timeout(busy, remaining)
                .map_err(|_| io::Error::other("SuanNIAO TLS write lock poisoned"))?;
        }
    }

    fn write_encrypted(&self, mut encrypted: &[u8], deadline: Instant) -> io::Result<()> {
        while !encrypted.is_empty() {
            self.ensure_open()?;
            // Bound every partial send by the same absolute deadline, rather
            // than granting each TLS record (or partial send) another 5 s.
            self.inner
                .writer
                .set_write_timeout(Some(remaining_write_time(deadline)?))?;
            let mut writer = &self.inner.writer;
            match writer.write(encrypted) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => encrypted = &encrypted[count..],
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(write_timeout());
                }
                Err(err) => return Err(err),
            }
        }
        remaining_write_time(deadline).map(|_| ())
    }

    fn flush_pending(&self) -> io::Result<()> {
        // Avoid waiting behind a busy application writer if nothing needs a
        // reply; it will also flush anything queued before its next record.
        if !lock(&self.inner.tls)?.wants_write() {
            return Ok(());
        }
        let deadline = Instant::now() + WRITE_TIMEOUT;
        let _writer = self.lock_writer(deadline)?;
        self.ensure_open()?;
        let encrypted = take_tls_output(&mut *lock(&self.inner.tls)?)?;
        let result = self.write_encrypted(&encrypted, deadline);
        if result.is_err() {
            self.abort();
        }
        result
    }

    fn ensure_open(&self) -> io::Result<()> {
        if self.inner.closed.load(Ordering::Acquire) {
            Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "TLS connection closed",
            ))
        } else {
            Ok(())
        }
    }

    /// Best-effort TLS close_notify followed by TCP shutdown. Never waits for a
    /// blocked writer's lock; if the peer stops reading, immediate TCP shutdown
    /// is used instead. Any clone may close the entire connection.
    pub fn shutdown(&self) {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        // The gate state is held only briefly; never wait for an active sender.
        let guard = self
            .inner
            .write_gate
            .busy
            .try_lock()
            .ok()
            .and_then(|mut busy| {
                if *busy {
                    None
                } else {
                    *busy = true;
                    Some(WriteGuard(&self.inner.write_gate))
                }
            });
        if let Some(_writer) = guard {
            if let Ok(mut tls) = self.inner.tls.try_lock() {
                tls.send_close_notify();
                if let Ok(encrypted) = take_tls_output(&mut tls) {
                    drop(tls);
                    // A single short close record, bounded even for a peer
                    // that has filled our TCP send buffer and stopped reading.
                    if self
                        .inner
                        .writer
                        .set_write_timeout(Some(Duration::from_millis(100)))
                        .is_ok()
                    {
                        let mut writer = &self.inner.writer;
                        let _ = writer.write_all(&encrypted);
                    }
                }
            }
        }
        self.abort();
    }

    pub fn peer_addr(&self) -> Option<SocketAddr> {
        self.inner.reader.peer_addr().ok()
    }

    fn abort(&self) {
        self.inner.closed.store(true, Ordering::Release);
        // Synchronize with the condition-variable wait to avoid a lost wakeup.
        if let Ok(_busy) = self.inner.write_gate.busy.lock() {
            self.inner.write_gate.ready.notify_all();
        }
        let _ = self.inner.reader.shutdown(Shutdown::Both);
        // On Windows shutdown prevents new socket operations but does not
        // necessarily wake recv/send already blocked on a duplicated handle.
        // Cancel pending operations on both handles *after* disabling new I/O.
        #[cfg(windows)]
        {
            cancel_pending_io(&self.inner.reader);
            cancel_pending_io(&self.inner.writer);
        }
    }
}

fn write_timeout() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "TLS write deadline expired")
}

fn remaining_write_time(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(write_timeout)
}

#[cfg(windows)]
fn configure_keepalive(tcp: &TcpStream) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    use windows::Win32::Networking::WinSock::{
        tcp_keepalive, WSAGetLastError, WSAIoctl, SIO_KEEPALIVE_VALS, SOCKET, SOCKET_ERROR,
    };
    let values = tcp_keepalive {
        onoff: 1,
        keepalivetime: 5_000,
        keepaliveinterval: 1_000,
    };
    let mut returned = 0;
    // The live stream owns this socket; both stack buffers remain valid for
    // this synchronous call. SIO_KEEPALIVE_VALS also enables SO_KEEPALIVE.
    let result = unsafe {
        WSAIoctl(
            SOCKET(tcp.as_raw_socket() as usize),
            SIO_KEEPALIVE_VALS,
            Some((&values as *const tcp_keepalive).cast()),
            std::mem::size_of_val(&values) as u32,
            None,
            0,
            &mut returned,
            None,
            None,
        )
    };
    if result == SOCKET_ERROR {
        Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }.0))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn cancel_pending_io(tcp: &TcpStream) {
    use std::os::windows::io::AsRawSocket;
    use windows::Win32::{Foundation::HANDLE, System::IO::CancelIoEx};
    // The live TcpStream owns the socket handle throughout this call. A null
    // OVERLAPPED pointer cancels all operations on it; ERROR_NOT_FOUND merely
    // means there was no outstanding I/O to cancel.
    unsafe {
        let _ = CancelIoEx(HANDLE(tcp.as_raw_socket() as *mut std::ffi::c_void), None);
    }
}

// Called only under write_gate once shared with the application; draining and
// writing in that order preserves TLS record sequence numbers across threads.
fn take_tls_output(tls: &mut ServerConnection) -> io::Result<Vec<u8>> {
    let mut encrypted = Vec::new();
    while tls.wants_write() {
        if tls.write_tls(&mut encrypted)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "TLS output made no progress",
            ));
        }
    }
    Ok(encrypted)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{net::TcpListener, os::windows::io::AsRawSocket, sync::mpsc, thread};
    use windows::{
        core::PSTR,
        Win32::Networking::WinSock::{
            getsockopt, IPPROTO_TCP, SOCKET, SOL_SOCKET, SO_KEEPALIVE, TCP_KEEPIDLE, TCP_KEEPINTVL,
        },
    };

    fn connection() -> (SecureConn, TcpStream) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (tcp, _) = listener.accept().unwrap();
        // These tests exercise local transport state without a TLS handshake.
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(rustls::server::ResolvesServerCertUsingSni::new()));
        let tls = ServerConnection::new(Arc::new(config)).unwrap();
        (SecureConn::new(tcp, tls).unwrap(), peer)
    }

    fn socket_option(tcp: &TcpStream, level: i32, option: i32) -> i32 {
        let mut value = 0_i32;
        let mut len = std::mem::size_of_val(&value) as i32;
        // getsockopt writes only to the live i32 buffer with its exact length.
        let result = unsafe {
            getsockopt(
                SOCKET(tcp.as_raw_socket() as usize),
                level,
                option,
                PSTR((&mut value as *mut i32).cast()),
                &mut len,
            )
        };
        assert_eq!(result, 0);
        // Winsock may return SO_KEEPALIVE as a one-byte boolean; the buffer
        // was zero-initialized, while the TCP timing options return a DWORD.
        assert!(matches!(len, 1 | 4));
        value
    }

    #[test]
    fn connection_enables_five_second_keepalive_and_one_second_probes() {
        let (conn, _peer) = connection();
        let tcp = &conn.inner.reader;
        assert_eq!(socket_option(tcp, SOL_SOCKET, SO_KEEPALIVE), 1);
        assert_eq!(socket_option(tcp, IPPROTO_TCP.0, TCP_KEEPIDLE), 5);
        assert_eq!(socket_option(tcp, IPPROTO_TCP.0, TCP_KEEPINTVL), 1);
        assert_eq!(tcp.read_timeout().unwrap(), None);
        assert_eq!(tcp.write_timeout().unwrap(), Some(WRITE_TIMEOUT));
        conn.shutdown();
    }

    #[test]
    fn write_deadline_includes_waiting_for_another_writer() {
        let (conn, peer) = connection();
        let held = conn.lock_writer(Instant::now() + WRITE_TIMEOUT).unwrap();
        let writer = conn.clone();
        let reader = conn.clone();
        let (write_tx, write_rx) = mpsc::channel();
        let (read_tx, read_rx) = mpsc::channel();
        let reader_thread = thread::spawn(move || {
            let _ = read_tx.send(reader.read(&mut [0]));
        });
        let writer_thread = thread::spawn(move || {
            let started = Instant::now();
            let result = writer.write_all(b"queued behind a busy writer");
            let _ = write_tx.send((started.elapsed(), result));
        });
        // No guard release or shutdown may be needed for the timeout to fire.
        let written = write_rx.recv_timeout(Duration::from_secs(7));
        let read = read_rx.recv_timeout(Duration::from_secs(1));
        drop(held);
        conn.shutdown();
        let _ = peer.shutdown(Shutdown::Both);
        writer_thread.join().unwrap();
        reader_thread.join().unwrap();
        let (elapsed, result) = written.expect("waiting for the write gate was unbounded");
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(elapsed >= Duration::from_secs(4));
        assert!(elapsed < Duration::from_secs(7));
        if let Ok(count) = read.expect("write gate timeout did not wake the reader") {
            assert_eq!(count, 0);
        }
        assert!(conn.write_all(b"closed").is_err());
    }
}
