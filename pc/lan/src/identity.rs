use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::digest::{digest, SHA256};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;
use std::sync::Arc;

use crate::storage::{atomic_create, canonical_base64, random_string, read_limited};

/// Stable computer ID and P-256 TLS certificate. The private key is DPAPI-protected on disk.
///
/// `identity.json` is one atomically replaced JSON object with `version: 1`,
/// `pc_id` (16 random bytes), `certificate_der` (public certificate DER), and
/// `private_key_dpapi` (current-user DPAPI ciphertext of PKCS#8 DER). All three
/// string fields use base64url without padding. Keeping the identity in one file
/// prevents a crash between writes from mixing a certificate with another key.
pub struct Identity {
    pub(crate) cert: CertificateDer<'static>,
    pub(crate) key: PrivateKeyDer<'static>,
    pc_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredIdentity {
    version: u32,
    pc_id: String,
    certificate_der: String,
    private_key_dpapi: String,
}

impl Identity {
    /// Load `identity.json`, or atomically create it if absent. Corruption/decryption
    /// errors are returned, never silently replaced with a different identity.
    /// DPAPI ties this identity to the current Windows user and machine.
    pub fn load_or_create(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("identity.json");
        match read_limited(&path, 1024 * 1024) {
            Ok(data) => Self::decode(&data),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let (identity, stored) = Self::generate()?;
                let data = serde_json::to_vec_pretty(&stored).map_err(io::Error::other)?;
                match atomic_create(&path, &data) {
                    Ok(()) => Ok(identity),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        Self::decode(&read_limited(&path, 1024 * 1024)?)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    pub fn pc_id(&self) -> &str {
        &self.pc_id
    }

    /// SHA-256 of the exact leaf certificate DER, base64url without padding (43 chars).
    pub fn fingerprint(&self) -> String {
        URL_SAFE_NO_PAD.encode(digest(&SHA256, self.cert.as_ref()).as_ref())
    }

    fn generate() -> io::Result<(Self, StoredIdentity)> {
        let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
            .map_err(io::Error::other)?;
        let mut params = rcgen::CertificateParams::new(vec!["suanniao.local".into()])
            .map_err(io::Error::other)?;
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "SuanNIAO LAN");
        params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let cert = params
            .self_signed(&key_pair)
            .map_err(io::Error::other)?
            .der()
            .clone();
        let private_der = key_pair.serialize_der();
        let encrypted = protect(&private_der)?;
        let pc_id = random_string(16)?;
        let stored = StoredIdentity {
            version: 1,
            pc_id: pc_id.clone(),
            certificate_der: URL_SAFE_NO_PAD.encode(cert.as_ref()),
            private_key_dpapi: URL_SAFE_NO_PAD.encode(encrypted),
        };
        Ok((
            Self {
                cert,
                key: PrivatePkcs8KeyDer::from(private_der).into(),
                pc_id,
            },
            stored,
        ))
    }

    fn decode(data: &[u8]) -> io::Result<Self> {
        let stored: StoredIdentity = serde_json::from_slice(data)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if stored.version != 1 || !canonical_base64(&stored.pc_id, 16) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid LAN identity version or ID",
            ));
        }
        let decode = |value: &str| {
            URL_SAFE_NO_PAD
                .decode(value)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        };
        let cert = CertificateDer::from(decode(&stored.certificate_der)?);
        let key: PrivateKeyDer<'static> =
            PrivatePkcs8KeyDer::from(unprotect(&decode(&stored.private_key_dpapi)?)?).into();
        // This also checks that the decrypted key matches the certificate.
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key.clone_key())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(Self {
            cert,
            key,
            pc_id: stored.pc_id,
        })
    }
}

#[cfg(windows)]
fn protect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    dpapi(bytes, false)
}

#[cfg(windows)]
fn unprotect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    dpapi(bytes, true)
}

#[cfg(windows)]
fn dpapi(bytes: &[u8], decrypt: bool) -> io::Result<Vec<u8>> {
    use windows::core::w;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes
            .len()
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "DPAPI input too large"))?,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // DPAPI reads input, allocates output with LocalAlloc, and runs without UI.
    // CRYPTPROTECT_LOCAL_MACHINE is intentionally absent: protection is per user.
    unsafe {
        if decrypt {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptProtectData(
                &input,
                w!("SuanNIAO LAN private key"),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
        .map_err(io::Error::other)?;
        let result = if output.cbData == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec()
        };
        // Erase the native plaintext allocation before releasing it.
        for offset in 0..output.cbData as usize {
            std::ptr::write_volatile(output.pbData.add(offset), 0);
        }
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(result)
    }
}

#[cfg(not(windows))]
fn protect(_: &[u8]) -> io::Result<Vec<u8>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "LAN identity storage requires Windows DPAPI",
    ))
}

#[cfg(not(windows))]
fn unprotect(_: &[u8]) -> io::Result<Vec<u8>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "LAN identity storage requires Windows DPAPI",
    ))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::storage::TestDir;

    #[test]
    fn identity_is_stable_and_private_key_is_dpapi_protected() {
        let dir = TestDir::new();
        let first = Identity::load_or_create(&dir.0).unwrap();
        let second = Identity::load_or_create(&dir.0).unwrap();
        assert_eq!(first.pc_id(), second.pc_id());
        assert_eq!(first.fingerprint(), second.fingerprint());
        assert_eq!(first.fingerprint().len(), 43);
        assert!(canonical_base64(first.pc_id(), 16));
        let stored: StoredIdentity =
            serde_json::from_slice(&std::fs::read(dir.0.join("identity.json")).unwrap()).unwrap();
        let encrypted = URL_SAFE_NO_PAD.decode(&stored.private_key_dpapi).unwrap();
        let plaintext = first.key.secret_der();
        assert_ne!(encrypted, plaintext);
        assert!(!encrypted
            .windows(plaintext.len())
            .any(|part| part == plaintext));
        assert_eq!(unprotect(&encrypted).unwrap(), plaintext);
        assert_eq!(
            URL_SAFE_NO_PAD.decode(stored.certificate_der).unwrap(),
            first.cert.as_ref()
        );
    }

    #[test]
    fn corrupt_identity_is_not_replaced() {
        let dir = TestDir::new();
        let path = dir.0.join("identity.json");
        std::fs::write(&path, b"incomplete JSON").unwrap();
        assert!(Identity::load_or_create(&dir.0).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"incomplete JSON");
    }

    #[test]
    fn corrupt_dpapi_blob_is_rejected() {
        let dir = TestDir::new();
        Identity::load_or_create(&dir.0).unwrap();
        let path = dir.0.join("identity.json");
        let mut stored: StoredIdentity =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        stored.private_key_dpapi = URL_SAFE_NO_PAD.encode(b"not a DPAPI blob");
        let corrupted = serde_json::to_vec(&stored).unwrap();
        std::fs::write(&path, &corrupted).unwrap();
        assert!(Identity::load_or_create(&dir.0).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), corrupted);
    }

    #[test]
    fn concurrent_identity_creation_keeps_one_identity() {
        let dir = TestDir::new();
        let mut threads = Vec::new();
        for _ in 0..4 {
            let path = dir.0.clone();
            threads.push(std::thread::spawn(move || {
                let identity = Identity::load_or_create(&path).unwrap();
                (identity.pc_id().to_owned(), identity.fingerprint())
            }));
        }
        let values: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(values.iter().all(|value| value == &values[0]));
    }
}
