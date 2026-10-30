//! Real loopback TLS tests. Every identity and phone database is in a temporary
//! directory; these tests never open the application's live data or executables.
#![cfg(windows)]

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{mpsc, Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::digest::{digest, SHA256};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};
use serde_json::{json, Value};
use suanniao_lan::{AuthedPhone, Identity, Pairings, SecureConn, Server};
use tempfile::TempDir;

type Client = StreamOwned<ClientConnection, TcpStream>;
type Accepted = Result<(SecureConn, AuthedPhone), String>;

/// Pin the leaf DER, and still cryptographically verify CertificateVerify.
/// The deliberately unrelated server name demonstrates there is no CA/name
/// dependency in this protocol.
#[derive(Debug)]
struct FingerprintVerifier(Vec<u8>);

impl ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if digest(&SHA256, end_entity.as_ref()).as_ref() == self.0 {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "certificate fingerprint mismatch".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

struct Fixture {
    dir: TempDir,
    identity: Identity,
    server: Arc<Server>,
    pairings: Arc<Pairings>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("suanniao-lan-test-")
            .tempdir()
            .unwrap();
        let identity = Identity::load_or_create(dir.path()).unwrap();
        let server = Arc::new(Server::new(&identity).unwrap());
        let pairings = Arc::new(Pairings::load(dir.path()).unwrap());
        Self {
            dir,
            identity,
            server,
            pairings,
        }
    }

    fn listen(&self) -> (SocketAddr, mpsc::Receiver<Accepted>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = Arc::clone(&self.server);
        let pairings = Arc::clone(&self.pairings);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let accepted = server
                .accept(tcp, &pairings)
                .map_err(|e| e.reason().to_owned());
            let _ = tx.send(accepted);
        });
        (addr, rx)
    }

    fn connect(&self) -> (Client, mpsc::Receiver<Accepted>) {
        let (addr, rx) = self.listen();
        (connect(addr, &self.identity.fingerprint()).unwrap(), rx)
    }

    fn pair(&self) -> (Client, SecureConn, AuthedPhone, Value) {
        let code = self.pairings.new_pair_code();
        let (mut client, rx) = self.connect();
        send_json(&mut client, pair_request(&code));
        let response = read_json(&mut client).unwrap();
        assert_eq!(response["t"], "paired");
        let (conn, phone) = accepted(rx);
        (client, conn, phone, response)
    }

    fn failure(&self, request: Value, reason: &str) {
        let (mut client, rx) = self.connect();
        send_json(&mut client, request);
        assert_eq!(
            read_json(&mut client).unwrap(),
            json!({"t":"authFail", "reason":reason})
        );
        assert_eq!(rejected(rx), reason);
        assert_closed(&mut client);
    }
}

fn connect(addr: SocketAddr, fingerprint: &str) -> io::Result<Client> {
    connect_with_versions(
        addr,
        fingerprint,
        &[&rustls::version::TLS13, &rustls::version::TLS12],
    )
}

fn connect_with_versions(
    addr: SocketAddr,
    fingerprint: &str,
    versions: &[&'static rustls::SupportedProtocolVersion],
) -> io::Result<Client> {
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(Duration::from_secs(15)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(15)))?;
    let config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(versions)
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(FingerprintVerifier(
                URL_SAFE_NO_PAD.decode(fingerprint).unwrap(),
            )))
            .with_no_client_auth();
    let connection = ClientConnection::new(
        Arc::new(config),
        ServerName::try_from("unrelated-to-certificate.invalid").unwrap(),
    )
    .unwrap();
    let mut client = StreamOwned::new(connection, tcp);
    while client.conn.is_handshaking() {
        client.conn.complete_io(&mut client.sock)?;
    }
    Ok(client)
}

fn pair_request(code: &str) -> Value {
    json!({"t":"pair", "code":code, "device":"测试手机", "model":"loopback"})
}

fn auth_request(response: &Value) -> Value {
    json!({"t":"auth", "id":response["id"], "token":response["token"]})
}

fn send_json(client: &mut Client, value: Value) {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    client.write_all(&bytes).unwrap();
    client.flush().unwrap();
}

fn read_json(client: &mut Client) -> io::Result<Value> {
    let mut line = Vec::new();
    loop {
        let mut byte = [0];
        client.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            return serde_json::from_slice(&line)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        }
        line.push(byte[0]);
        assert!(line.len() <= 65536, "unbounded response");
    }
}

fn accepted(rx: mpsc::Receiver<Accepted>) -> (SecureConn, AuthedPhone) {
    match rx.recv_timeout(Duration::from_secs(15)).unwrap() {
        Ok(accepted) => accepted,
        Err(reason) => panic!("unexpected rejection: {reason}"),
    }
}

fn rejected(rx: mpsc::Receiver<Accepted>) -> String {
    match rx.recv_timeout(Duration::from_secs(15)).unwrap() {
        Ok(_) => panic!("unexpected authentication success"),
        Err(reason) => reason,
    }
}

fn assert_closed(client: &mut Client) {
    match client.read(&mut [0]) {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::BrokenPipe
            ) => {}
        other => panic!("connection was not closed: {other:?}"),
    }
}

#[test]
fn pair_persist_reload_then_authenticate() {
    let mut fx = Fixture::new();
    let before = fx.pairings.changed();
    let (client, conn, phone, response) = fx.pair();
    assert!(phone.newly_paired);
    assert_eq!(phone.name, "测试手机");
    assert_eq!(phone.model, "loopback");
    assert_eq!(response["id"], phone.id);
    assert_eq!(response["pc"], fx.identity.pc_id());
    assert_eq!(
        URL_SAFE_NO_PAD
            .decode(response["token"].as_str().unwrap())
            .unwrap()
            .len(),
        32
    );
    assert_eq!(URL_SAFE_NO_PAD.decode(&phone.id).unwrap().len(), 16);
    assert_eq!(
        URL_SAFE_NO_PAD.decode(fx.identity.pc_id()).unwrap().len(),
        16
    );
    assert_eq!(fx.identity.fingerprint().len(), 43);
    assert!(fx.pairings.changed() > before);
    let phones = fx.pairings.phones();
    assert_eq!(phones.len(), 1);
    assert!(phones[0].paired_at > 0);
    assert!(phones[0].last_seen >= phones[0].paired_at);
    let disk = std::fs::read_to_string(fx.dir.path().join("phones.json")).unwrap();
    assert!(
        !disk.contains(response["token"].as_str().unwrap()),
        "raw token written to disk"
    );
    assert_eq!(
        URL_SAFE_NO_PAD
            .decode(&phones[0].token_sha256)
            .unwrap()
            .len(),
        32
    );
    conn.shutdown();
    drop(client);
    fx.pairings = Arc::new(Pairings::load(fx.dir.path()).unwrap());
    let identity = Identity::load_or_create(fx.dir.path()).unwrap();
    assert_eq!(identity.pc_id(), fx.identity.pc_id());
    assert_eq!(identity.fingerprint(), fx.identity.fingerprint());
    let (mut client, rx) = fx.connect();
    send_json(&mut client, auth_request(&response));
    assert_eq!(
        read_json(&mut client).unwrap(),
        json!({"t":"authOk", "pc":fx.identity.pc_id()})
    );
    let (conn, phone2) = accepted(rx);
    assert!(!phone2.newly_paired);
    assert_eq!(phone.id, phone2.id);
    assert_eq!(
        client.conn.protocol_version(),
        Some(rustls::ProtocolVersion::TLSv1_3)
    );
    conn.shutdown();
}

#[test]
fn tls12_clients_can_pair_and_exchange_application_data() {
    let fx = Fixture::new();
    let code = fx.pairings.new_pair_code();
    let (addr, rx) = fx.listen();
    let mut client =
        connect_with_versions(addr, &fx.identity.fingerprint(), &[&rustls::version::TLS12])
            .unwrap();
    send_json(&mut client, pair_request(&code));
    assert_eq!(read_json(&mut client).unwrap()["t"], "paired");
    assert_eq!(
        client.conn.protocol_version(),
        Some(rustls::ProtocolVersion::TLSv1_2)
    );
    let (conn, _) = accepted(rx);
    conn.write_all(b"server message").unwrap();
    let mut received = [0; 14];
    client.read_exact(&mut received).unwrap();
    assert_eq!(&received, b"server message");
    conn.shutdown();
}

#[test]
fn wrong_replaced_cancelled_and_reused_codes_are_rejected() {
    let fx = Fixture::new();
    let code = fx.pairings.new_pair_code();
    assert!(URL_SAFE_NO_PAD.decode(&code).unwrap().len() >= 16);
    fx.failure(pair_request("wrong"), "badCode");
    let replacement = fx.pairings.new_pair_code();
    fx.failure(pair_request(&code), "badCode");
    fx.pairings.cancel_pair_code();
    fx.failure(pair_request(&replacement), "badCode");
    let code = fx.pairings.new_pair_code();
    let (mut client, rx) = fx.connect();
    send_json(&mut client, pair_request(&code));
    assert_eq!(read_json(&mut client).unwrap()["t"], "paired");
    let (conn, _) = accepted(rx);
    conn.shutdown();
    fx.failure(pair_request(&code), "badCode");
    assert_eq!(fx.pairings.phones().len(), 1);
}

#[test]
fn bad_token_and_removed_phone_are_rejected() {
    let fx = Fixture::new();
    let (_, conn, phone, response) = fx.pair();
    conn.shutdown();
    let mut bad = auth_request(&response);
    bad["token"] = json!(URL_SAFE_NO_PAD.encode([0u8; 32]));
    fx.failure(bad, "badToken");
    let before = fx.pairings.changed();
    fx.pairings.remove(&phone.id).unwrap();
    assert!(fx.pairings.changed() > before);
    assert!(fx.pairings.phones().is_empty());
    fx.failure(auth_request(&response), "unknown");
    assert!(Pairings::load(fx.dir.path()).unwrap().phones().is_empty());
}

#[test]
fn repairing_with_previous_token_replaces_only_the_proven_record() {
    let fx = Fixture::new();
    let (_, old_conn, old_phone, old_response) = fx.pair();
    old_conn.shutdown();
    let (_, other_conn, other_phone, _) = fx.pair();
    other_conn.shutdown();
    let code = fx.pairings.new_pair_code();
    let mut request = pair_request(&code);
    request["prev"] = json!({"id": old_phone.id, "token": old_response["token"]});
    let (mut client, rx) = fx.connect();
    send_json(&mut client, request);
    let response = read_json(&mut client).unwrap();
    assert_eq!(response["t"], "paired");
    let (conn, phone) = accepted(rx);
    assert!(phone.newly_paired);
    assert_ne!(phone.id, old_phone.id);
    conn.shutdown();
    let phones = Pairings::load(fx.dir.path()).unwrap().phones();
    assert_eq!(phones.len(), 2);
    assert!(phones.iter().any(|record| record.id == other_phone.id));
    assert!(phones.iter().any(|record| record.id == phone.id));
    fx.failure(auth_request(&old_response), "unknown");
    let (mut client, rx) = fx.connect();
    send_json(&mut client, auth_request(&response));
    assert_eq!(read_json(&mut client).unwrap()["t"], "authOk");
    accepted(rx).0.shutdown();
}

#[test]
fn repairing_with_wrong_or_absent_previous_token_keeps_old_record() {
    let fx = Fixture::new();
    let (_, conn, old_phone, old_response) = fx.pair();
    conn.shutdown();
    for prev in [
        Some(json!({"id": old_phone.id, "token": URL_SAFE_NO_PAD.encode([0u8; 32])})),
        Some(json!({"id": "unknown", "token": old_response["token"]})),
        Some(json!({"id": old_phone.id})),
        None,
    ] {
        let before = fx.pairings.phones().len();
        let code = fx.pairings.new_pair_code();
        let mut request = pair_request(&code);
        if let Some(prev) = prev {
            request["prev"] = prev;
        }
        let (mut client, rx) = fx.connect();
        send_json(&mut client, request);
        assert_eq!(read_json(&mut client).unwrap()["t"], "paired");
        accepted(rx).0.shutdown();
        let phones = Pairings::load(fx.dir.path()).unwrap().phones();
        assert_eq!(phones.len(), before + 1);
        assert!(phones.iter().any(|record| record.id == old_phone.id));
    }
    let (mut client, rx) = fx.connect();
    send_json(&mut client, auth_request(&old_response));
    assert_eq!(read_json(&mut client).unwrap()["t"], "authOk");
    accepted(rx).0.shutdown();
}

#[test]
fn mismatched_fingerprint_fails_during_tls_handshake() {
    let fx = Fixture::new();
    let (addr, rx) = fx.listen();
    let result = connect(addr, &URL_SAFE_NO_PAD.encode([0u8; 32]));
    assert!(result.is_err(), "untrusted certificate accepted");
    assert_eq!(rejected(rx), "protocol");
    assert!(fx.pairings.phones().is_empty());
}

#[test]
fn malformed_or_oversized_first_lines_fail_closed() {
    let fx = Fixture::new();
    for request in [json!({"t":"move", "dx":3}), json!({"t":"auth"}), json!([])] {
        fx.failure(request, "protocol");
    }
    for bytes in [b"not json\n".as_slice(), b"{\"t\":\"\xff\"}\n".as_slice()] {
        let (mut client, rx) = fx.connect();
        client.write_all(bytes).unwrap();
        client.flush().unwrap();
        assert_eq!(read_json(&mut client).unwrap()["reason"], "protocol");
        assert_eq!(rejected(rx), "protocol");
        assert_closed(&mut client);
    }
    let (mut client, rx) = fx.connect();
    // No newline: the size limit itself must terminate the request.
    let _ = client.write_all(&vec![b' '; 65537]);
    let _ = client.flush();
    assert_eq!(read_json(&mut client).unwrap()["reason"], "protocol");
    assert_eq!(rejected(rx), "protocol");
    assert_closed(&mut client);
}

#[test]
fn authentication_preserves_already_received_application_bytes() {
    let fx = Fixture::new();
    let code = fx.pairings.new_pair_code();
    let (mut client, rx) = fx.connect();
    let mut packet = serde_json::to_vec(&pair_request(&code)).unwrap();
    let suffix = b"{\"t\":\"hello\"}\n{\"t\":\"ping\"}\n";
    packet.push(b'\n');
    packet.extend_from_slice(suffix);
    client.write_all(&packet).unwrap();
    client.flush().unwrap();
    assert_eq!(read_json(&mut client).unwrap()["t"], "paired");
    let (conn, _) = accepted(rx);
    assert_eq!(conn.peer_addr(), Some(client.sock.local_addr().unwrap()));
    let (tx, rx) = mpsc::channel();
    let reader = conn.clone();
    thread::spawn(move || {
        let mut actual = vec![0; suffix.len()];
        let mut offset = 0;
        while offset < actual.len() {
            let n = reader.read(&mut actual[offset..]).unwrap();
            assert_ne!(n, 0);
            offset += n;
        }
        let _ = tx.send(actual);
    });
    let result = rx.recv_timeout(Duration::from_secs(3));
    conn.shutdown();
    assert_eq!(result.unwrap(), suffix);
}

#[test]
fn incoming_large_lines_and_adjacent_records_preserve_every_byte() {
    for version in [&rustls::version::TLS13, &rustls::version::TLS12] {
        let fx = Fixture::new();
        let code = fx.pairings.new_pair_code();
        let (addr, accepted_rx) = fx.listen();
        let mut client =
            connect_with_versions(addr, &fx.identity.fingerprint(), &[version]).unwrap();
        send_json(&mut client, pair_request(&code));
        assert_eq!(read_json(&mut client).unwrap()["t"], "paired");
        let (conn, _) = accepted(accepted_rx);
        let lines = vec![
            "aB012345".repeat(16 * 1024 / 8 + 512 / 8) + "\n",
            "first small line\n".to_owned(),
            "中文剪贴板\n".to_owned(),
            "\n".to_owned(),
            "0123456789ABCDEF".repeat(200 * 1024 / 16) + "\n",
            "last small line\n".to_owned(),
        ];
        // Queue distinct records, then send their ciphertext together. This
        // places a record's tail and the next record in the same TCP read.
        client.conn.set_buffer_limit(None);
        for line in &lines {
            client.conn.writer().write_all(line.as_bytes()).unwrap();
        }
        let mut encrypted = Vec::new();
        while client.conn.wants_write() {
            client.conn.write_tls(&mut encrypted).unwrap();
        }
        let reader = conn.clone();
        let (tx, rx) = mpsc::channel();
        let reader_thread = thread::spawn(move || {
            let result = (|| -> io::Result<Vec<Vec<u8>>> {
                let mut actual = Vec::new();
                let mut line = Vec::new();
                let mut buffer = [0; 317];
                while actual.len() < 6 {
                    let count = reader.read(&mut buffer)?;
                    if count == 0 {
                        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
                    }
                    for &byte in &buffer[..count] {
                        line.push(byte);
                        if byte == b'\n' {
                            actual.push(std::mem::take(&mut line));
                        }
                    }
                }
                assert!(line.is_empty());
                Ok(actual)
            })();
            let _ = tx.send(result);
        });
        let sent = client.sock.write_all(&encrypted);
        let received = rx.recv_timeout(Duration::from_secs(4));
        conn.shutdown();
        reader_thread.join().unwrap();
        sent.unwrap();
        let expected: Vec<_> = lines.into_iter().map(String::into_bytes).collect();
        assert_eq!(received.unwrap().unwrap(), expected);
    }
}

#[test]
fn write_deadline_aborts_backpressure_and_wakes_blocked_reader() {
    let fx = Fixture::new();
    let (client, conn, _, _) = fx.pair();
    let reader = conn.clone();
    let writer = conn.clone();
    let (read_tx, read_rx) = mpsc::channel();
    let (write_tx, write_rx) = mpsc::channel();
    let reader_thread = thread::spawn(move || {
        let _ = read_tx.send(reader.read(&mut [0]));
    });
    let writer_thread = thread::spawn(move || {
        let data = vec![b'x'; 16 * 1024 * 1024];
        let started = Instant::now();
        let result = writer.write_all(&data);
        let _ = write_tx.send((started.elapsed(), result));
    });
    // The peer stays connected but never drains application data.
    assert!(matches!(
        read_rx.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let written = write_rx.recv_timeout(Duration::from_secs(8));
    let read = read_rx.recv_timeout(Duration::from_secs(1));
    // Always clean up before asserting, even for an unbounded implementation.
    conn.shutdown();
    let _ = client.sock.shutdown(std::net::Shutdown::Both);
    reader_thread.join().unwrap();
    writer_thread.join().unwrap();
    let (elapsed, result) = written.expect("write_all exceeded its five-second deadline");
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert!(
        elapsed >= Duration::from_secs(4),
        "write timed out too early"
    );
    assert!(
        elapsed < Duration::from_secs(7),
        "write deadline was extended"
    );
    if let Ok(count) = read.expect("timed-out write did not wake the reader") {
        assert_eq!(count, 0);
    }
    assert!(conn
        .write_all(b"cannot reuse a partial TLS record")
        .is_err());
}

#[test]
fn blocked_read_does_not_block_concurrent_atomic_writes() {
    let fx = Fixture::new();
    let (mut client, conn, _, _) = fx.pair();
    let reader = conn.clone();
    let (read_tx, read_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    thread::spawn(move || {
        started_tx.send(()).unwrap();
        let mut byte = [0];
        let result = reader.read(&mut byte).map(|n| (n, byte[0]));
        let _ = read_tx.send(result);
    });
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    thread::sleep(Duration::from_millis(100));
    let count = 160 * 1024;
    let barrier = Arc::new(Barrier::new(3));
    let (write_tx, write_rx) = mpsc::channel();
    for byte in [b'a', b'b'] {
        let writer = conn.clone();
        let barrier = Arc::clone(&barrier);
        let tx = write_tx.clone();
        thread::spawn(move || {
            barrier.wait();
            let _ = tx.send(writer.write_all(&vec![byte; count]));
        });
    }
    barrier.wait();
    client
        .sock
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    let mut received = vec![0; count * 2];
    let result = client.read_exact(&mut received);
    if let Err(error) = result {
        conn.shutdown();
        panic!("writes blocked behind the idle read: {error}");
    }
    let first = received[0];
    assert!(first == b'a' || first == b'b');
    assert!(received[..count].iter().all(|&x| x == first));
    assert!(received[count..]
        .iter()
        .all(|&x| x == if first == b'a' { b'b' } else { b'a' }));
    for _ in 0..2 {
        write_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap();
    }
    client.write_all(b"x").unwrap();
    client.flush().unwrap();
    let result = read_rx.recv_timeout(Duration::from_secs(3));
    conn.shutdown();
    assert_eq!(result.unwrap().unwrap(), (1, b'x'));
}

#[test]
fn shutdown_wakes_a_blocked_reader() {
    let fx = Fixture::new();
    let (mut client, conn, _, _) = fx.pair();
    let reader = conn.clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(reader.read(&mut [0]));
    });
    thread::sleep(Duration::from_millis(100));
    conn.shutdown();
    match rx.recv_timeout(Duration::from_secs(3)).unwrap() {
        Ok(n) => assert_eq!(n, 0),
        Err(_) => {}
    }
    assert_closed(&mut client);
}

#[test]
fn shutdown_wakes_both_backpressured_writer_and_blocked_reader() {
    let fx = Fixture::new();
    let (client, conn, _, _) = fx.pair();
    let reader = conn.clone();
    let writer = conn.clone();
    let (read_tx, read_rx) = mpsc::channel();
    let (write_tx, write_rx) = mpsc::channel();
    let (start_tx, start_rx) = mpsc::channel();
    let read_started = start_tx.clone();
    thread::spawn(move || {
        read_started.send(()).unwrap();
        let _ = read_tx.send(reader.read(&mut [0]));
    });
    thread::spawn(move || {
        // Bounded allocation, much larger than an idle loopback receive window.
        let data = vec![b'x'; 16 * 1024 * 1024];
        start_tx.send(()).unwrap();
        let _ = write_tx.send(writer.write_all(&data));
    });
    for _ in 0..2 {
        start_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    }
    // The client deliberately neither sends nor reads any application bytes.
    assert!(matches!(
        read_rx.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert!(matches!(
        write_rx.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let closer = conn.clone();
    let (closed_tx, closed_rx) = mpsc::channel();
    thread::spawn(move || {
        closer.shutdown();
        let _ = closed_tx.send(());
    });
    let closed = closed_rx.recv_timeout(Duration::from_secs(3));
    let read = read_rx.recv_timeout(Duration::from_secs(3));
    let write = write_rx.recv_timeout(Duration::from_secs(3));
    // Ensure even a failing implementation cannot leave the peer open forever.
    let _ = client.sock.shutdown(std::net::Shutdown::Both);
    closed.expect("shutdown blocked behind a writer");
    match read.expect("shutdown did not cancel the blocked reader") {
        Ok(n) => assert_eq!(n, 0),
        Err(_) => {}
    }
    assert!(write
        .expect("shutdown did not cancel the blocked writer")
        .is_err());
}

#[test]
fn handshake_authentication_and_slow_drip_share_a_bounded_deadline() {
    let fx = Fixture::new();
    let (mut authed_client, authed_conn, _, _) = fx.pair();
    let reader = authed_conn.clone();
    let (authed_tx, authed_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut byte = [0];
        let _ = authed_tx.send(reader.read(&mut byte).map(|n| (n, byte[0])));
    });
    let start = Instant::now();
    let (raw_addr, raw_rx) = fx.listen();
    let _raw_client = TcpStream::connect(raw_addr).unwrap();
    let (mut idle_client, idle_rx) = fx.connect();
    let (mut drip_client, drip_rx) = fx.connect();
    let dripper = thread::spawn(move || {
        // A reset-on-every-read timeout would take ~19 seconds in this case.
        for _ in 0..45 {
            drip_client.write_all(b" ").unwrap();
            drip_client.flush().unwrap();
            thread::sleep(Duration::from_millis(200));
        }
        read_json(&mut drip_client).unwrap()
    });
    assert_eq!(rejected(raw_rx), "timeout");
    assert_eq!(read_json(&mut idle_client).unwrap()["reason"], "timeout");
    assert_eq!(rejected(idle_rx), "timeout");
    assert_eq!(rejected(drip_rx), "timeout");
    assert_eq!(dripper.join().unwrap()["reason"], "timeout");
    assert!(start.elapsed() >= Duration::from_secs(9));
    assert!(
        start.elapsed() < Duration::from_secs(14),
        "timeout was extended by partial input"
    );
    // Unlike authentication, an established idle session has no read timeout.
    authed_client.write_all(b"x").unwrap();
    authed_client.flush().unwrap();
    let resumed = authed_rx.recv_timeout(Duration::from_secs(3));
    authed_conn.shutdown();
    assert_eq!(resumed.unwrap().unwrap(), (1, b'x'));
    assert_eq!(fx.pairings.phones().len(), 1);
}
