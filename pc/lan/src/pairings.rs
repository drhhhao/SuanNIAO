use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::digest::{digest, SHA256};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;

use crate::storage::{atomic_write, canonical_base64, random_string, read_limited};

/// An element of the `phones.json` array. `id` encodes 16 random bytes as
/// base64url without padding. `token_sha256` is unpadded
/// base64url(SHA-256(token's UTF-8 bytes)), never the token itself. `paired_at`
/// and `last_seen` are Unix seconds; `name` and `model` are the phone's UTF-8 labels.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PhoneRecord {
    pub id: String,
    pub name: String,
    pub model: String,
    pub token_sha256: String,
    pub paired_at: u64,
    pub last_seen: u64,
}

struct PairCode {
    hash: [u8; 32],
    expires: Instant,
}

struct State {
    phones: Vec<PhoneRecord>,
    code: Option<PairCode>,
    revision: u64,
}

/// One shared instance per storage directory; clone an `Arc<Pairings>` for threads.
/// Pair codes are memory-only and expire using a monotonic clock. File mutations
/// complete before in-memory changes become visible. Cross-process sharing is not supported.
pub struct Pairings {
    file: PathBuf,
    state: Mutex<State>,
}

impl Pairings {
    pub fn load(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let file = dir.join("phones.json");
        let phones: Vec<PhoneRecord> = match read_limited(&file, 16 * 1024 * 1024) {
            Ok(data) => serde_json::from_slice(&data)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        let mut ids = HashSet::new();
        for phone in &phones {
            if !canonical_base64(&phone.id, 16)
                || !canonical_base64(&phone.token_sha256, 32)
                || !ids.insert(phone.id.clone())
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid or duplicate LAN phone record",
                ));
            }
        }
        Ok(Self {
            file,
            state: Mutex::new(State {
                phones,
                code: None,
                revision: 0,
            }),
        })
    }

    /// Make a 128-bit one-use code valid for ten minutes, replacing any old code.
    /// Panics only if the OS CSPRNG fails; use `try_new_pair_code` to handle that error.
    pub fn new_pair_code(&self) -> String {
        self.try_new_pair_code()
            .expect("operating system random generator failed")
    }

    pub fn try_new_pair_code(&self) -> io::Result<String> {
        let code = random_string(16)?;
        let mut state = self.lock();
        state.code = Some(PairCode {
            hash: hash(&code),
            expires: Instant::now() + Duration::from_secs(600),
        });
        state.revision = state.revision.wrapping_add(1);
        Ok(code)
    }

    pub fn cancel_pair_code(&self) {
        let mut state = self.lock();
        if state.code.take().is_some() {
            state.revision = state.revision.wrapping_add(1);
        }
    }

    pub fn phones(&self) -> Vec<PhoneRecord> {
        self.lock().phones.clone()
    }

    /// Unknown IDs are a no-op. Existing live sessions must be closed by the caller.
    pub fn remove(&self, id: &str) -> io::Result<()> {
        let mut state = self.lock();
        let Some(index) = state.phones.iter().position(|phone| phone.id == id) else {
            return Ok(());
        };
        let mut phones = state.phones.clone();
        phones.remove(index);
        self.save(&phones)?;
        state.phones = phones;
        state.revision = state.revision.wrapping_add(1);
        Ok(())
    }

    /// Monotonic modulo u64 counter for this instance, including pair-code changes.
    pub fn changed(&self) -> u64 {
        self.lock().revision
    }

    pub(crate) fn pair(
        &self,
        code: &str,
        name: &str,
        model: &str,
        prev: Option<(&str, &str)>,
    ) -> io::Result<Result<(PhoneRecord, String), &'static str>> {
        let mut state = self.lock();
        let supplied_hash = hash(code);
        let valid = state.code.as_ref().is_some_and(|active| {
            bool::from(active.hash.ct_eq(&supplied_hash)) && Instant::now() < active.expires
        });
        if !valid {
            return Ok(Err("badCode"));
        }
        let id = loop {
            let id = random_string(16)?;
            if state.phones.iter().all(|phone| phone.id != id) {
                break id;
            }
        };
        let token = random_string(32)?;
        let now = unix_seconds();
        let phone = PhoneRecord {
            id,
            name: name.into(),
            model: model.into(),
            token_sha256: URL_SAFE_NO_PAD.encode(hash(&token)),
            paired_at: now,
            last_seen: now,
        };
        let mut phones = state.phones.clone();
        // Replacing a pairing requires proof of the old token as well as a
        // valid new pair code. Save the removal and insertion atomically, so
        // any failure leaves the old pairing and the new code usable.
        if let Some((id, token)) = prev {
            if let Some(index) = phones.iter().position(|phone| phone.id == id) {
                let expected = URL_SAFE_NO_PAD.decode(&phones[index].token_sha256);
                if expected.is_ok_and(|expected| {
                    bool::from(expected.as_slice().ct_eq(&hash(token)))
                        && canonical_base64(token, 32)
                }) {
                    phones.remove(index);
                }
            }
        }
        phones.push(phone.clone());
        self.save(&phones)?;
        state.phones = phones;
        state.code = None;
        state.revision = state.revision.wrapping_add(1);
        Ok(Ok((phone, token)))
    }

    pub(crate) fn authenticate(
        &self,
        id: &str,
        token: &str,
    ) -> io::Result<Result<PhoneRecord, &'static str>> {
        let mut state = self.lock();
        let Some(index) = state.phones.iter().position(|phone| phone.id == id) else {
            return Ok(Err("unknown"));
        };
        let expected = URL_SAFE_NO_PAD
            .decode(&state.phones[index].token_sha256)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        // Digest comparison is constant time; malformed token encodings still fail.
        if !bool::from(expected.as_slice().ct_eq(&hash(token))) || !canonical_base64(token, 32) {
            return Ok(Err("badToken"));
        }
        let now = unix_seconds();
        if state.phones[index].last_seen != now {
            let mut phones = state.phones.clone();
            phones[index].last_seen = now;
            self.save(&phones)?;
            state.phones = phones;
            state.revision = state.revision.wrapping_add(1);
        }
        Ok(Ok(state.phones[index].clone()))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // All mutations are transactional; a panic must fail closed rather than
        // allow authentication using potentially inconsistent state.
        self.state.lock().expect("LAN pairing state lock poisoned")
    }

    fn save(&self, phones: &[PhoneRecord]) -> io::Result<()> {
        let data = serde_json::to_vec_pretty(phones).map_err(io::Error::other)?;
        if data.len() > 16 * 1024 * 1024 {
            return Err(io::Error::other("LAN phone store exceeds 16 MiB"));
        }
        atomic_write(&self.file, &data)
    }
}

fn hash(value: &str) -> [u8; 32] {
    digest(&SHA256, value.as_bytes())
        .as_ref()
        .try_into()
        .expect("SHA-256 length")
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::TestDir;
    use std::sync::{Arc, Barrier};

    #[test]
    fn pair_persists_only_token_hash_and_authenticates_after_reload() {
        let dir = TestDir::new();
        let pairings = Pairings::load(&dir.0).unwrap();
        let code = pairings.new_pair_code();
        let (phone, token) = pairings
            .pair(&code, "小米 13", "2211133C", None)
            .unwrap()
            .unwrap();
        assert!(canonical_base64(&phone.id, 16));
        assert!(canonical_base64(&token, 32));
        let file = std::fs::read_to_string(dir.0.join("phones.json")).unwrap();
        assert!(!file.contains(&token));
        assert!(file.contains(&URL_SAFE_NO_PAD.encode(hash(&token))));
        let reloaded = Pairings::load(&dir.0).unwrap();
        let result = reloaded.authenticate(&phone.id, &token).unwrap().unwrap();
        assert_eq!(result.name, "小米 13");
        assert_eq!(result.model, "2211133C");
        assert_eq!(
            reloaded
                .authenticate(&phone.id, &random_string(32).unwrap())
                .unwrap(),
            Err("badToken")
        );
        reloaded.remove(&phone.id).unwrap();
        assert_eq!(
            reloaded.authenticate(&phone.id, &token).unwrap(),
            Err("unknown")
        );
        assert!(Pairings::load(&dir.0).unwrap().phones().is_empty());
    }

    #[test]
    fn wrong_expired_replaced_cancelled_and_reused_codes_fail() {
        let dir = TestDir::new();
        let pairings = Pairings::load(&dir.0).unwrap();
        let first = pairings.new_pair_code();
        assert_eq!(
            pairings.pair("incorrect", "phone", "model", None).unwrap(),
            Err("badCode")
        );
        let second = pairings.new_pair_code();
        assert_eq!(
            pairings.pair(&first, "phone", "model", None).unwrap(),
            Err("badCode")
        );
        pairings.lock().code.as_mut().unwrap().expires = Instant::now() - Duration::from_secs(1);
        assert_eq!(
            pairings.pair(&second, "phone", "model", None).unwrap(),
            Err("badCode")
        );
        let third = pairings.new_pair_code();
        pairings.cancel_pair_code();
        assert_eq!(
            pairings.pair(&third, "phone", "model", None).unwrap(),
            Err("badCode")
        );
        let fourth = pairings.new_pair_code();
        assert!(pairings
            .pair(&fourth, "phone", "model", None)
            .unwrap()
            .is_ok());
        assert_eq!(
            pairings.pair(&fourth, "phone", "model", None).unwrap(),
            Err("badCode")
        );
    }

    #[test]
    fn concurrent_pairing_consumes_code_exactly_once() {
        let dir = TestDir::new();
        let pairings = Arc::new(Pairings::load(&dir.0).unwrap());
        let code = pairings.new_pair_code();
        let barrier = Arc::new(Barrier::new(8));
        let mut threads = Vec::new();
        for _ in 0..8 {
            let pairings = Arc::clone(&pairings);
            let barrier = Arc::clone(&barrier);
            let code = code.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                pairings
                    .pair(&code, "phone", "model", None)
                    .unwrap()
                    .is_ok()
            }));
        }
        let successes = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|success| *success)
            .count();
        assert_eq!(successes, 1);
        assert_eq!(pairings.phones().len(), 1);
    }

    #[test]
    fn failed_save_neither_authorizes_nor_consumes_code() {
        let dir = TestDir::new();
        let pairings = Pairings::load(&dir.0).unwrap();
        let code = pairings.new_pair_code();
        // An isolated directory at the destination forces atomic replacement to fail.
        let file = dir.0.join("phones.json");
        std::fs::create_dir(&file).unwrap();
        let revision = pairings.changed();
        assert!(pairings.pair(&code, "phone", "model", None).is_err());
        assert!(pairings.phones().is_empty());
        assert_eq!(pairings.changed(), revision);
        std::fs::remove_dir(&file).unwrap();
        assert!(pairings
            .pair(&code, "phone", "model", None)
            .unwrap()
            .is_ok());
    }

    #[test]
    fn failed_removal_keeps_prior_authorization() {
        let dir = TestDir::new();
        let pairings = Pairings::load(&dir.0).unwrap();
        let code = pairings.new_pair_code();
        let (phone, token) = pairings
            .pair(&code, "phone", "model", None)
            .unwrap()
            .unwrap();
        let path = dir.0.join("phones.json");
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(pairings.remove(&phone.id).is_err());
        assert_eq!(pairings.phones(), vec![phone.clone()]);
        std::fs::remove_dir(&path).unwrap();
        assert!(pairings.authenticate(&phone.id, &token).unwrap().is_ok());
    }

    #[test]
    fn valid_previous_pairing_is_replaced_and_persisted() {
        let dir = TestDir::new();
        let pairings = Pairings::load(&dir.0).unwrap();
        let (old, old_token) = pairings
            .pair(&pairings.new_pair_code(), "old", "model", None)
            .unwrap()
            .unwrap();
        let (other, other_token) = pairings
            .pair(&pairings.new_pair_code(), "other", "model", None)
            .unwrap()
            .unwrap();
        let code = pairings.new_pair_code();
        let revision = pairings.changed();
        let (new, new_token) = pairings
            .pair(&code, "new", "model", Some((&old.id, &old_token)))
            .unwrap()
            .unwrap();
        assert_ne!(new.id, old.id);
        assert_ne!(new_token, old_token);
        assert_eq!(pairings.changed(), revision.wrapping_add(1));
        assert_eq!(pairings.phones(), vec![other.clone(), new.clone()]);
        let reloaded = Pairings::load(&dir.0).unwrap();
        assert_eq!(reloaded.phones(), pairings.phones());
        assert_eq!(
            reloaded.authenticate(&old.id, &old_token).unwrap(),
            Err("unknown")
        );
        assert!(reloaded.authenticate(&new.id, &new_token).unwrap().is_ok());
        assert!(reloaded
            .authenticate(&other.id, &other_token)
            .unwrap()
            .is_ok());
        let file = std::fs::read_to_string(dir.0.join("phones.json")).unwrap();
        assert!(!file.contains(&old_token));
        assert!(!file.contains(&new_token));
        assert_eq!(
            pairings.pair(&code, "reuse", "model", None).unwrap(),
            Err("badCode")
        );
    }

    #[test]
    fn missing_or_invalid_previous_pairing_keeps_old_records() {
        let dir = TestDir::new();
        let pairings = Pairings::load(&dir.0).unwrap();
        let (old, old_token) = pairings
            .pair(&pairings.new_pair_code(), "old", "model", None)
            .unwrap()
            .unwrap();
        let wrong_token = random_string(32).unwrap();
        let unknown_id = random_string(16).unwrap();
        for prev in [
            None,
            Some((old.id.as_str(), wrong_token.as_str())),
            Some((old.id.as_str(), "malformed token")),
            Some((unknown_id.as_str(), old_token.as_str())),
        ] {
            let before = pairings.phones();
            let (new, token) = pairings
                .pair(&pairings.new_pair_code(), "new", "model", prev)
                .unwrap()
                .unwrap();
            let after = pairings.phones();
            assert_eq!(&after[..before.len()], before.as_slice());
            assert_eq!(after.len(), before.len() + 1);
            assert!(pairings.authenticate(&new.id, &token).unwrap().is_ok());
            assert!(pairings.authenticate(&old.id, &old_token).unwrap().is_ok());
        }
        assert_eq!(Pairings::load(&dir.0).unwrap().phones(), pairings.phones());
    }

    #[test]
    fn previous_pairing_cannot_be_removed_with_invalid_or_expired_code() {
        let dir = TestDir::new();
        let pairings = Pairings::load(&dir.0).unwrap();
        let (old, old_token) = pairings
            .pair(&pairings.new_pair_code(), "old", "model", None)
            .unwrap()
            .unwrap();
        let code = pairings.new_pair_code();
        let revision = pairings.changed();
        assert_eq!(
            pairings
                .pair("incorrect", "new", "model", Some((&old.id, &old_token)))
                .unwrap(),
            Err("badCode")
        );
        pairings.lock().code.as_mut().unwrap().expires = Instant::now() - Duration::from_secs(1);
        assert_eq!(
            pairings
                .pair(&code, "new", "model", Some((&old.id, &old_token)))
                .unwrap(),
            Err("badCode")
        );
        assert_eq!(pairings.changed(), revision);
        assert_eq!(pairings.phones(), vec![old]);
        assert_eq!(Pairings::load(&dir.0).unwrap().phones(), pairings.phones());
    }

    #[test]
    fn failed_replacement_keeps_old_pairing_and_new_code() {
        let dir = TestDir::new();
        let pairings = Pairings::load(&dir.0).unwrap();
        let (old, old_token) = pairings
            .pair(&pairings.new_pair_code(), "old", "model", None)
            .unwrap()
            .unwrap();
        let code = pairings.new_pair_code();
        let revision = pairings.changed();
        let path = dir.0.join("phones.json");
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(pairings
            .pair(&code, "new", "model", Some((&old.id, &old_token)))
            .is_err());
        assert_eq!(pairings.changed(), revision);
        assert_eq!(pairings.phones(), vec![old.clone()]);
        std::fs::remove_dir(&path).unwrap();
        let (new, _) = pairings
            .pair(&code, "new", "model", Some((&old.id, &old_token)))
            .unwrap()
            .unwrap();
        assert_eq!(pairings.phones(), vec![new]);
        assert_eq!(Pairings::load(&dir.0).unwrap().phones(), pairings.phones());
    }

    #[test]
    fn corrupt_phone_store_is_rejected_without_overwrite() {
        let dir = TestDir::new();
        let path = dir.0.join("phones.json");
        std::fs::write(&path, b"{}").unwrap();
        assert!(Pairings::load(&dir.0).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{}");
    }
}
