//! 日志、内存统计等小工具。日志写到 `%APPDATA%\PhonePad\logs\core.log`：超过 5 MB 就轮转，
//! 旧的依次叫 core.1.log（最近的）… core.5.log，最多留 5 个（和 axonkey 一样 5 MB × 5）。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::GetCurrentProcess;

/// 当前的日志文件和已经写了多少字节
static LOG_FILE: Mutex<Option<(File, u64)>> = Mutex::new(None);
const LOG_MAX_BYTES: u64 = 5_000_000;
const LOG_KEEP: usize = 5;

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::util::log_line(&format!($($arg)*)) };
}

fn log_dir() -> std::path::PathBuf {
    phonepad_config::data_dir().join("logs")
}

/// 打开（必要时先轮转）日志文件
fn open_log() -> Option<(File, u64)> {
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("core.log");
    // 以前的版本超过 2 MB 换成 core.old.log：并进轮转里
    let old = dir.join("core.old.log");
    if old.exists() && !dir.join("core.1.log").exists() {
        let _ = std::fs::rename(&old, dir.join("core.1.log"));
    }
    if std::fs::metadata(&path).map(|m| m.len() >= LOG_MAX_BYTES).unwrap_or(false) {
        rotate(&dir);
    }
    let file = OpenOptions::new().create(true).append(true).open(&path).ok()?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    Some((file, size))
}

/// core.log → core.1.log，core.1.log → core.2.log……最老的删掉
fn rotate(dir: &std::path::Path) {
    let _ = std::fs::remove_file(dir.join(format!("core.{LOG_KEEP}.log")));
    for n in (1..LOG_KEEP).rev() {
        let _ = std::fs::rename(dir.join(format!("core.{n}.log")), dir.join(format!("core.{}.log", n + 1)));
    }
    let _ = std::fs::rename(dir.join("core.log"), dir.join("core.1.log"));
}

pub fn init_log() {
    *LOG_FILE.lock().unwrap() = open_log();
}

pub fn log_line(msg: &str) {
    let t = unsafe { GetLocalTime() };
    let line = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} {msg}\n",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    );
    print!("{line}");
    let mut log = LOG_FILE.lock().unwrap();
    if let Some((file, size)) = log.as_mut() {
        if file.write_all(line.as_bytes()).is_ok() {
            *size += line.len() as u64;
        }
        if *size >= LOG_MAX_BYTES {
            // 写满了：关掉、轮转、开个新的
            *log = None;
            rotate(&log_dir());
            *log = open_log();
        }
    }
}

/// 本地时间比 UTC 早多少秒（配置备份的文件名用本地时间）
pub fn local_offset_secs() -> i64 {
    use windows::Win32::System::SystemInformation::GetSystemTime;
    let (local, utc) = unsafe { (GetLocalTime(), GetSystemTime()) };
    let minutes = |t: &windows::Win32::Foundation::SYSTEMTIME| (t.wDay as i64 * 24 + t.wHour as i64) * 60 + t.wMinute as i64;
    // 日期跨月时按差值最小的那个算
    let mut offset = minutes(&local) - minutes(&utc);
    for wrap in [-28 * 1440, -29 * 1440, -30 * 1440, -31 * 1440, 28 * 1440, 29 * 1440, 30 * 1440, 31 * 1440] {
        if (offset + wrap).abs() < offset.abs() {
            offset += wrap;
        }
    }
    offset * 60
}

/// 本地日期，例如 2026-10-29（「今天」的用量按这个换天）
pub fn local_day() -> String {
    let t = unsafe { GetLocalTime() };
    format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay)
}

/// 返回（工作集，私有内存），单位字节。
pub fn memory_bytes() -> (u64, u64) {
    let mut counters = PROCESS_MEMORY_COUNTERS::default();
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    unsafe {
        let _ = GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size);
    }
    (counters.WorkingSetSize as u64, counters.PagefileUsage as u64)
}

pub fn memory() -> String {
    let (working_set, private) = memory_bytes();
    format!("内存：工作集 {:.1} MB，私有 {:.1} MB", mb(working_set), mb(private))
}

pub fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

pub fn host_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into())
}

/// 日志里显示的文字预览，太长就截断。
pub fn preview(text: &str) -> String {
    let shown: String = text.chars().take(40).collect();
    if text.chars().count() > 40 { format!("{shown}…") } else { shown }
}

#[cfg(test)]
mod tests {
    /// 轮转：最新的旧日志是 core.1.log，最多留 5 个，再老的删掉
    #[test]
    fn rotation_keeps_five() {
        let dir = std::env::temp_dir().join(format!("phonepad-logtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for round in 0..7 {
            std::fs::write(dir.join("core.log"), format!("round {round}")).unwrap();
            super::rotate(&dir);
        }
        assert!(!dir.join("core.log").exists());
        assert_eq!(std::fs::read_to_string(dir.join("core.1.log")).unwrap(), "round 6");
        assert_eq!(std::fs::read_to_string(dir.join("core.5.log")).unwrap(), "round 2");
        assert!(!dir.join("core.6.log").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
