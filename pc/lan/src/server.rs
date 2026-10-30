use std::{
    fmt,
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
    sync::{mpsc, Arc},
    thread,
    time::{Duration, Instant},
};

use rustls::{ServerConfig, ServerConnection};
use serde::Deserialize;
use serde_json::json;

use crate::{Identity, Pairings, PhoneRecord, SecureConn};

const ACCEPT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_AUTH_LINE: usize = 64 * 1024;

/// Authenticated identity returned only after sending `paired` or `authOk`.
#[derive(Debug, Clone)]
pub struct AuthedPhone {
    pub id: String,
    pub name: String,
    pub model: String,
    pub newly_paired: bool,
}

/// A rejected TLS/authentication connection, already closed by `accept`.
/// `reason()` is one of `badCode`, `unknown`, `badToken`, `timeout`, `protocol`.
/// Local I/O details are available through `std::error::Error::source`, and are
/// never sent to the peer. No authentication secrets appear in this error.
#[derive(Debug)]
pub struct AcceptError {
    reason: &'static str,
    source: Option<io::Error>,
}

impl AcceptError {
    pub fn reason(&self) -> &'static str {
        self.reason
    }

    fn rejected(reason: &'static str) -> Self {
        Self {
            reason,
            source: None,
        }
    }
}

impl From<io::Error> for AcceptError {
    fn from(source: io::Error) -> Self {
        let reason = match source.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => "timeout",
            _ => "protocol",
        };
        Self {
            reason,
            source: Some(source),
        }
    }
}

impl fmt::Display for AcceptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LAN authentication failed: {}", self.reason)
    }
}

impl std::error::Error for AcceptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|source| source as _)
    }
}

/// Share via `Arc` or clone; each accepted connection has its own TLS state.
#[derive(Clone)]
pub struct Server {
    config: Arc<ServerConfig>,
    pc_id: String,
}

#[derive(Deserialize)]
#[serde(tag = "t", deny_unknown_fields)]
enum AuthRequest {
    #[serde(rename = "pair")]
    Pair {
        code: String,
        device: String,
        model: String,
        #[serde(default, deserialize_with = "previous_pairing")]
        prev: Option<PreviousPairing>,
    },
    #[serde(rename = "auth")]
    Auth { id: String, token: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviousPairing {
    id: String,
    token: String,
}

// This is only a best-effort cleanup hint. An old or malformed credential must
// never prevent a valid new pairing from succeeding.
fn previous_pairing<'de, D>(deserializer: D) -> Result<Option<PreviousPairing>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

impl Server {
    pub fn new(identity: &Identity) -> io::Result<Self> {
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(io::Error::other)?
                .with_no_client_auth()
                .with_single_cert(vec![identity.cert.clone()], identity.key.clone_key())
                .map_err(io::Error::other)?;
        Ok(Self {
            config: Arc::new(config),
            pc_id: identity.pc_id().to_owned(),
        })
    }

    /// Complete TLS and the first-line authentication under one 10-second
    /// deadline. Failed connections receive `authFail` when TLS is available,
    /// then close. The best-effort failure notification gets at most 100 ms.
    /// Successful connections have TCP_NODELAY and Windows TCP keepalive on
    /// (5-second idle, 1-second interval), no idle read timeout, and a 5-second
    /// write deadline.
    pub fn accept(
        &self,
        mut tcp: TcpStream,
        pairings: &Pairings,
    ) -> Result<(SecureConn, AuthedPhone), AcceptError> {
        let deadline = Instant::now() + ACCEPT_TIMEOUT;
        let mut tls = ServerConnection::new(self.config.clone()).map_err(io::Error::other)?;
        tls.set_buffer_limit(Some(64 * 1024));
        let result = (|| {
            tcp.set_nonblocking(false)?;
            tcp.set_nodelay(true)?;
            while tls.is_handshaking() {
                tls.complete_io(&mut DeadlineTcp {
                    tcp: &mut tcp,
                    deadline,
                })?;
            }
            check_deadline(deadline)?;
            let line = read_auth_line(&mut tls, &mut tcp, deadline)?;
            let request: AuthRequest =
                serde_json::from_slice(&line).map_err(|_| AcceptError::rejected("protocol"))?;
            check_deadline(deadline)?;
            let (record, newly_paired, response) = storage_before_deadline(&tcp, deadline, || {
                Ok(match request {
                    AuthRequest::Pair {
                        code,
                        device,
                        model,
                        prev,
                    } => {
                        let (record, token) = pairings
                            .pair(
                                &code,
                                &device,
                                &model,
                                prev.as_ref()
                                    .map(|prev| (prev.id.as_str(), prev.token.as_str())),
                            )?
                            .map_err(AcceptError::rejected)?;
                        let response = json!({
                            "t": "paired", "id": record.id, "token": token, "pc": self.pc_id
                        });
                        (record, true, response)
                    }
                    AuthRequest::Auth { id, token } => {
                        let record = pairings
                            .authenticate(&id, &token)?
                            .map_err(AcceptError::rejected)?;
                        (record, false, json!({ "t": "authOk", "pc": self.pc_id }))
                    }
                })
            })?;
            check_deadline(deadline)?;
            send_json(&mut tls, &mut tcp, deadline, &response)?;
            check_deadline(deadline)?;
            tcp.set_read_timeout(None)?;
            tcp.set_write_timeout(None)?;
            Ok::<_, AcceptError>(authed_phone(record, newly_paired))
        })();

        match result {
            Ok(phone) => Ok((SecureConn::new(tcp, tls)?, phone)),
            Err(err) => {
                if !tls.is_handshaking() {
                    let notice_deadline = Instant::now() + Duration::from_millis(100);
                    let _ = send_json(
                        &mut tls,
                        &mut tcp,
                        notice_deadline,
                        &json!({ "t": "authFail", "reason": err.reason() }),
                    );
                    tls.send_close_notify();
                    let _ = flush_tls(&mut tls, &mut tcp, notice_deadline);
                }
                let _ = tcp.shutdown(Shutdown::Both);
                Err(err)
            }
        }
    }
}

fn authed_phone(record: PhoneRecord, newly_paired: bool) -> AuthedPhone {
    AuthedPhone {
        id: record.id,
        name: record.name,
        model: record.model,
        newly_paired,
    }
}

// Socket I/O enforces its own remaining timeout. Disk writes and waiting for a
// Pairings lock cannot be interrupted that way, so close TCP at the same
// deadline even if local storage is stalled. This short-lived watchdog exists
// only while authenticating a complete, syntactically valid request. The caller
// must still cap concurrent accepts. A disk operation already in flight may
// finish later, but can never return an authenticated live connection afterward.
fn storage_before_deadline<T>(
    tcp: &TcpStream,
    deadline: Instant,
    operation: impl FnOnce() -> Result<T, AcceptError>,
) -> Result<T, AcceptError> {
    let watchdog_tcp = tcp.try_clone()?;
    let (done, receiver) = mpsc::channel();
    let watchdog = thread::Builder::new()
        .name("suanniao-lan-auth-deadline".into())
        .spawn(move || {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if receiver.recv_timeout(remaining) == Err(mpsc::RecvTimeoutError::Timeout) {
                let _ = watchdog_tcp.shutdown(Shutdown::Both);
            }
        })?;
    let result = operation();
    let _ = done.send(());
    let _ = watchdog.join();
    check_deadline(deadline)?;
    result
}

fn read_auth_line(
    tls: &mut ServerConnection,
    tcp: &mut TcpStream,
    deadline: Instant,
) -> Result<Vec<u8>, AcceptError> {
    let mut line = Vec::new();
    loop {
        check_deadline(deadline)?;
        let mut byte = [0];
        match tls.reader().read(&mut byte) {
            Ok(0) => return Err(AcceptError::rejected("protocol")),
            Ok(_) => {
                if byte[0] == b'\n' {
                    return Ok(line);
                }
                line.push(byte[0]);
                if line.len() >= MAX_AUTH_LINE {
                    return Err(AcceptError::rejected("protocol"));
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                flush_tls(tls, tcp, deadline)?;
                if tls.read_tls(&mut DeadlineTcp { tcp, deadline })? == 0 {
                    return Err(AcceptError::rejected("protocol"));
                }
                tls.process_new_packets().map_err(io::Error::other)?;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err.into()),
        }
    }
}

fn send_json(
    tls: &mut ServerConnection,
    tcp: &mut TcpStream,
    deadline: Instant,
    value: &serde_json::Value,
) -> io::Result<()> {
    check_deadline(deadline)?;
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    tls.writer().write_all(&line)?;
    flush_tls(tls, tcp, deadline)
}

fn flush_tls(tls: &mut ServerConnection, tcp: &mut TcpStream, deadline: Instant) -> io::Result<()> {
    while tls.wants_write() {
        check_deadline(deadline)?;
        if tls.write_tls(&mut DeadlineTcp { tcp, deadline })? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "TLS output made no progress",
            ));
        }
    }
    Ok(())
}

fn check_deadline(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "TLS/authentication deadline expired",
            )
        })
}

/// Recompute the remaining budget before *each* socket operation. A peer cannot
/// extend the deadline by dripping partial TLS records or an unfinished line.
struct DeadlineTcp<'a> {
    tcp: &'a mut TcpStream,
    deadline: Instant,
}

impl Read for DeadlineTcp<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.tcp
            .set_read_timeout(Some(check_deadline(self.deadline)?))?;
        self.tcp.read(buf)
    }
}

impl Write for DeadlineTcp<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tcp
            .set_write_timeout(Some(check_deadline(self.deadline)?))?;
        self.tcp.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        check_deadline(self.deadline)?;
        self.tcp.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn pair_request_accepts_optional_previous_credentials() {
        let request = json!({
            "t": "pair", "code": "code", "device": "phone", "model": "model"
        });
        let AuthRequest::Pair { prev, .. } = serde_json::from_value(request.clone()).unwrap()
        else {
            panic!("expected pair request");
        };
        assert!(prev.is_none());
        let mut request_with_prev = request;
        request_with_prev["prev"] = json!({"id": "old-id", "token": "old-token"});
        let AuthRequest::Pair { prev, .. } = serde_json::from_value(request_with_prev).unwrap()
        else {
            panic!("expected pair request");
        };
        let prev = prev.unwrap();
        assert_eq!(prev.id, "old-id");
        assert_eq!(prev.token, "old-token");
    }

    #[test]
    fn malformed_previous_credentials_do_not_reject_pair_request() {
        for prev in [
            json!(null),
            json!("invalid"),
            json!([]),
            json!({}),
            json!({"id": "old-id"}),
            json!({"id": "old-id", "token": 42}),
            json!({"id": "old-id", "token": "old-token", "extra": true}),
        ] {
            let request = json!({
                "t": "pair", "code": "code", "device": "phone", "model": "model", "prev": prev
            });
            let AuthRequest::Pair { prev, .. } = serde_json::from_value(request).unwrap() else {
                panic!("expected pair request");
            };
            assert!(prev.is_none());
        }
    }

    #[test]
    fn stalled_storage_closes_peer_before_the_operation_returns() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let (tcp, _) = listener.accept().unwrap();
        let (tx, rx) = mpsc::channel();
        let peer_thread = thread::spawn(move || {
            tx.send(peer.read(&mut [0])).unwrap();
        });
        let deadline = Instant::now() + Duration::from_millis(100);
        let result = storage_before_deadline(&tcp, deadline, || {
            // Simulate storage blocked until well after its deadline. The peer
            // must already observe closure while this operation is still here.
            assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap(), 0);
            Ok(())
        });
        assert_eq!(result.unwrap_err().reason(), "timeout");
        peer_thread.join().unwrap();
    }
}
