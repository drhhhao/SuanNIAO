use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::rand::{SecureRandom, SystemRandom};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub(crate) fn random_string(bytes: usize) -> io::Result<String> {
    let mut value = vec![0; bytes];
    SystemRandom::new()
        .fill(&mut value)
        .map_err(|_| io::Error::other("operating system random generator failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(value))
}

pub(crate) fn canonical_base64(value: &str, size: usize) -> bool {
    URL_SAFE_NO_PAD
        .decode(value)
        .is_ok_and(|bytes| bytes.len() == size && URL_SAFE_NO_PAD.encode(bytes) == value)
}

pub(crate) fn read_limited(path: &Path, max: u64) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    fs::File::open(path)?.take(max + 1).read_to_end(&mut data)?;
    if data.len() as u64 > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "LAN state file too large",
        ));
    }
    Ok(data)
}

/// Flush a same-directory temporary file, then atomically replace the destination.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_save(path, bytes, true)
}

/// Identity creation must not replace an identity made by another initializer.
pub(crate) fn atomic_create(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_save(path, bytes, false)
}

fn atomic_save(path: &Path, bytes: &[u8], replace: bool) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent directory"))?;
    let temporary = parent.join(format!(".suanniao-lan-{}.tmp", random_string(16)?));
    let cleanup = TemporaryFile(temporary);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&cleanup.0)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    replace_file(&cleanup.0, path, replace)?;
    Ok(())
}

struct TemporaryFile(PathBuf);

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path, replace: bool) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let flags = if replace {
        MOVEFILE_WRITE_THROUGH | MOVEFILE_REPLACE_EXISTING
    } else {
        MOVEFILE_WRITE_THROUGH
    };
    // Both terminated UTF-16 paths remain alive for this synchronous Win32 call.
    unsafe { MoveFileExW(PCWSTR(source.as_ptr()), PCWSTR(destination.as_ptr()), flags) }.map_err(
        |error| {
            // Preserve ERROR_FILE_EXISTS / ERROR_ALREADY_EXISTS for racing identity creators.
            if matches!(error.code().0 as u32, 0x80070050 | 0x800700B7) {
                io::Error::new(io::ErrorKind::AlreadyExists, error)
            } else {
                io::Error::other(error)
            }
        },
    )
}

#[cfg(test)]
pub(crate) struct TestDir(pub(crate) PathBuf);

#[cfg(test)]
impl TestDir {
    pub(crate) fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("suanniao-lan-test-{}", random_string(16).unwrap()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

#[cfg(test)]
impl Drop for TestDir {
    fn drop(&mut self) {
        // Delete only the isolated random child created by this helper.
        if self.0.parent() == Some(std::env::temp_dir().as_path())
            && self
                .0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("suanniao-lan-test-")
        {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path, replace: bool) -> io::Result<()> {
    if replace {
        fs::rename(source, destination)
    } else {
        // A same-filesystem hard link installs a complete file without replacement.
        fs::hard_link(source, destination)
    }
}
