//! TLS and QR-authorized pairing for SuanNIAO LAN connections.
//!
//! The caller owns the TCP listener, QR UI and application protocol. Initialize
//! one [`Identity`] and one shared [`Pairings`] for a storage directory, then:
//!
//! ```no_run
//! use std::{io, net::TcpListener, path::Path, sync::Arc};
//! use suanniao_lan::{Identity, Pairings, Server};
//!
//! # fn main() -> io::Result<()> {
//! let dir = Path::new("example-suanniao-lan"); // caller's app data directory
//! let identity = Identity::load_or_create(dir)?;
//! let pairings = Arc::new(Pairings::load(dir)?);
//! let server = Arc::new(Server::new(&identity)?);
//! let code = pairings.new_pair_code();
//! // Put identity.pc_id(), identity.fingerprint(), code and TCP address in QR.
//! // Closing the QR should call pairings.cancel_pair_code().
//! let listener = TcpListener::bind("0.0.0.0:0")?;
//! for tcp in listener.incoming() {
//!     let (server, pairings) = (server.clone(), pairings.clone());
//!     let tcp = tcp?;
//!     std::thread::spawn(move || {
//!         if let Ok((conn, phone)) = server.accept(tcp, &pairings) {
//!             // Authentication response has already been sent. Only now may
//!             // the caller accept input/control messages from this phone.
//!             let writer = conn.clone();
//!             writer.write_all(b"{\"t\":\"hello\"}\n").unwrap();
//!             let mut buf = [0; 4096];
//!             while let Ok(n) = conn.read(&mut buf) {
//!                 if n == 0 { break; }
//!                 // Feed buf[..n] to the caller's newline-delimited protocol.
//!             }
//!             conn.shutdown();
//!         }
//!     });
//! }
//! # Ok(()) }
//! ```
//!
//! TLS 1.3 is preferred, TLS 1.2 is supported; the provider is explicitly ring.
//! Peers must pin SHA-256 of the leaf certificate DER, encoded as unpadded
//! base64url. No client certificate is required: the first encrypted JSON line
//! must be `pair` or `auth`. TLS plus authentication has a single 10-second
//! deadline and a 64-KiB authentication line limit (including the newline).
//! Extra plaintext after that line remains available through [`SecureConn::read`].
//! On success TCP reads have no idle timeout; Windows TCP keepalive probes start
//! after 5 idle seconds and repeat every second. Each application write has a
//! total 5-second deadline; failure closes the connection and wakes readers.
//! Message limits, listener concurrency limits, and closing existing sessions after
//! [`Pairings::remove`] are the caller's responsibility.
//!
//! Share the same `Pairings` via `Arc` among threads; separate instances/processes
//! are not a synchronized database. The pairing code is in memory only, is
//! replaced on generation, expires after 10 minutes and is consumed once.
//! Persistent identity is Windows-only (current-user DPAPI); no plaintext private
//! key fallback is provided. See [`Identity`] and [`PhoneRecord`] for file formats.
//! If local storage stalls, a watchdog closes TCP at the authentication deadline;
//! the synchronous storage operation itself may return later. If a successful
//! pairing response is lost, generate a fresh QR code and pair again.

mod connection;
mod identity;
mod pairings;
mod server;
mod storage;

pub use connection::SecureConn;
pub use identity::Identity;
pub use pairings::{Pairings, PhoneRecord};
pub use server::{AcceptError, AuthedPhone, Server};
