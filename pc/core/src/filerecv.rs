//! 手机发来的文件（在相册、微信里「分享 → 蒜鸟」，或者手机菜单里「发文件到电脑」）。
//!
//! 手机 → 电脑：fileStart（id、batch、index、count、name、size、mime；size 是 -1 = 手机也不知道多大）
//!              → 一串 filePart（id、d＝Base64）→ fileEnd（id）；中途不要了发 fileCancel（id）。
//! 电脑 → 手机（从收到的那条连接回）：fileAck（id、got＝已经写好的字节数；got 0 = 可以开始发；出错带 error，手机就不发了）、
//!              fileDone（id、ok、name＝存成的文件名，或者 error）。
//! 手机发出去的数据最多比 got 领先一个窗口（局域网 256 KB，蓝牙 12 KB）：连接里不会堆一大截文件数据，
//! 触控板、按键的消息随时插得进去。
//!
//! - 存到设置里的文件夹（默认是系统「下载」文件夹里的「蒜鸟」）。先写成「名字.蒜鸟接收中」，收齐、大小对上才改成正式名字，
//!   重名的加「 (1)」「 (2)」……
//! - 一次分享的几个文件（一批）都收完：设置里开着的话放进剪贴板（只有一张图片就放图片，直接粘进聊天框；别的放文件，
//!   粘进文件夹、聊天软件），托盘弹通知，点通知在文件夹里选中它。
//! - 连接断了：这条连接上没收完的文件删掉。

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{json, Value};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, KNOWN_FOLDER_FLAG};

use crate::{clipboard, clipsync, log, state, tray};

/// 正在收的一个文件
struct Incoming {
    batch: u64,
    index: u64,
    count: u64,
    name: String,
    /// 手机说的大小；None = 不知道
    size: Option<u64>,
    got: u64,
    dir: PathBuf,
    temp: PathBuf,
    out: BufWriter<File>,
    started: Instant,
}

/// 一条连接上正在收的文件（会话结束时丢掉，没收完的删掉）
pub struct Receiver {
    via: &'static str,
    files: HashMap<u64, Incoming>,
}

/// 一批文件：收完了几个（第几个、存成的路径），失败了几个
struct Batch {
    count: u64,
    done: Vec<(u64, PathBuf)>,
    failed: u64,
    at: Instant,
}

static BATCHES: Mutex<Option<HashMap<u64, Batch>>> = Mutex::new(None);

/// 电脑上至少留这么多空间：别把盘写满
const KEEP_FREE: u64 = 200 << 20;

impl Receiver {
    pub fn new(via: &'static str) -> Self {
        Self { via, files: HashMap::new() }
    }

    /// 处理 fileStart / filePart / fileEnd / fileCancel，返回要回给手机的消息
    pub fn handle(&mut self, kind: &str, msg: &Value) -> Option<Value> {
        let id = msg.get("id").and_then(Value::as_u64)?;
        match kind {
            "fileStart" => Some(match self.start(id, msg) {
                Ok(()) => json!({"t": "fileAck", "id": id, "got": 0}),
                Err(error) => {
                    log!("📥 没收手机发来的文件：{error}");
                    json!({"t": "fileAck", "id": id, "error": error})
                }
            }),
            "filePart" => {
                let incoming = self.files.get_mut(&id)?;
                let result = base64::engine::general_purpose::STANDARD
                    .decode(msg.get("d").and_then(Value::as_str).unwrap_or(""))
                    .map_err(|_| "数据坏了".to_string())
                    .and_then(|bytes| {
                        incoming.got += bytes.len() as u64;
                        if incoming.size.is_some_and(|size| incoming.got > size) {
                            return Err("比说好的大小多".into());
                        }
                        incoming.out.write_all(&bytes).map_err(|e| format!("写不进去：{e}"))
                    });
                Some(match result {
                    Ok(()) => json!({"t": "fileAck", "id": id, "got": incoming.got}),
                    Err(error) => {
                        let name = incoming.name.clone();
                        self.fail(id);
                        log!("📥 {name} 没收完：{error}");
                        json!({"t": "fileAck", "id": id, "error": error})
                    }
                })
            }
            "fileEnd" => Some(match self.finish(id) {
                Ok(name) => json!({"t": "fileDone", "id": id, "ok": true, "name": name}),
                Err(error) => json!({"t": "fileDone", "id": id, "ok": false, "error": error}),
            }),
            "fileCancel" => {
                if let Some(incoming) = self.files.get(&id) {
                    log!("📥 手机取消了发送：{}", incoming.name);
                }
                self.fail(id);
                None
            }
            _ => None,
        }
    }

    fn start(&mut self, id: u64, msg: &Value) -> Result<(), String> {
        let num = |k: &str| msg.get(k).and_then(Value::as_u64);
        let name = clean_name(msg.get("name").and_then(Value::as_str).unwrap_or(""));
        let size = msg.get("size").and_then(Value::as_i64).filter(|s| *s >= 0).map(|s| s as u64);
        let dir = folder();
        std::fs::create_dir_all(&dir).map_err(|e| format!("建不了文件夹 {}：{e}", dir.display()))?;
        if let (Some(size), Some(free)) = (size, free_space(&dir)) {
            if size + KEEP_FREE > free {
                return Err(format!("电脑上 {} 所在的盘空间不够（还剩 {}）", dir.display(), human(free)));
            }
        }
        let mut temp = dir.join(format!("{name}.蒜鸟接收中"));
        if temp.exists() {
            temp = dir.join(format!("{name}.{id}.蒜鸟接收中"));
        }
        let file = File::create(&temp).map_err(|e| format!("建不了文件：{e}"))?;
        let (batch, index, count) = (num("batch").unwrap_or(id), num("index").unwrap_or(0), num("count").unwrap_or(1).max(1));
        {
            let mut batches = BATCHES.lock().unwrap();
            let batches = batches.get_or_insert_with(HashMap::new);
            batches.retain(|_, b| b.at.elapsed() < Duration::from_secs(6 * 3600));
            batches.entry(batch).or_insert_with(|| Batch { count, done: Vec::new(), failed: 0, at: Instant::now() });
        }
        if let Some(old) = self.files.remove(&id) {
            drop(old.out);
            let _ = std::fs::remove_file(&old.temp);
        }
        self.files.insert(
            id,
            Incoming {
                batch,
                index,
                count,
                name,
                size,
                got: 0,
                dir,
                temp,
                out: BufWriter::with_capacity(256 << 10, file),
                started: Instant::now(),
            },
        );
        Ok(())
    }

    /// 收齐了：改成正式名字。返回存成的文件名
    fn finish(&mut self, id: u64) -> Result<String, String> {
        let Some(mut incoming) = self.files.remove(&id) else { return Err("没在收这个文件".into()) };
        let result = (|| {
            incoming.out.flush().map_err(|e| format!("写不进去：{e}"))?;
            if let Some(size) = incoming.size {
                if incoming.got != size {
                    return Err(format!("只收到 {}，应该是 {}", human(incoming.got), human(size)));
                }
            }
            let target = unique(&incoming.dir, &incoming.name);
            drop(incoming.out);
            std::fs::rename(&incoming.temp, &target).map_err(|e| format!("改不了名字：{e}"))?;
            Ok(target)
        })();
        match result {
            Ok(target) => {
                let secs = incoming.started.elapsed().as_secs_f64();
                let saved = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                log!(
                    "📥 收到手机发来的 {saved}（{}，{}用时 {secs:.1} 秒，约 {}/s）",
                    human(incoming.got),
                    self.via,
                    human((incoming.got as f64 / secs.max(0.001)) as u64)
                );
                batch_progress(incoming.batch, incoming.count, Some((incoming.index, target)));
                Ok(saved)
            }
            Err(error) => {
                let _ = std::fs::remove_file(&incoming.temp);
                log!("📥 {} 没收好：{error}", incoming.name);
                batch_progress(incoming.batch, incoming.count, None);
                Err(error)
            }
        }
    }

    fn fail(&mut self, id: u64) {
        if let Some(incoming) = self.files.remove(&id) {
            drop(incoming.out);
            let _ = std::fs::remove_file(&incoming.temp);
            batch_progress(incoming.batch, incoming.count, None);
        }
    }
}

impl Drop for Receiver {
    /// 连接断了：没收完的删掉
    fn drop(&mut self) {
        let ids: Vec<u64> = self.files.keys().copied().collect();
        for id in ids {
            if let Some(incoming) = self.files.get(&id) {
                log!("📥 连接断了，{} 没收完（收到 {}），已删掉", incoming.name, human(incoming.got));
            }
            self.fail(id);
        }
    }
}

/// 一批里又好了（或者失败了）一个；都有结果了就放进剪贴板、弹通知
fn batch_progress(batch: u64, count: u64, done: Option<(u64, PathBuf)>) {
    let finished = {
        let mut batches = BATCHES.lock().unwrap();
        let batches = batches.get_or_insert_with(HashMap::new);
        let entry = batches.entry(batch).or_insert_with(|| Batch { count, done: Vec::new(), failed: 0, at: Instant::now() });
        match done {
            Some(item) => entry.done.push(item),
            None => entry.failed += 1,
        }
        if entry.done.len() as u64 + entry.failed >= entry.count {
            batches.remove(&batch)
        } else {
            None
        }
    };
    if let Some(mut batch) = finished {
        batch.done.sort_by_key(|(index, _)| *index);
        let paths: Vec<PathBuf> = batch.done.into_iter().map(|(_, path)| path).collect();
        if !paths.is_empty() {
            // 解码大照片要一会儿，别占着连接的读线程
            std::thread::spawn(move || deliver(paths, batch.failed));
        }
    }
}

/// 一批收完：放进剪贴板、弹通知
fn deliver(paths: Vec<PathBuf>, failed: u64) {
    let settings = state::config().settings.receive;
    let mut clip = "";
    if settings.clipboard {
        let seq = match paths.as_slice() {
            [one] if is_image(one) => match load_image(one) {
                Some((rgba, png)) => {
                    clip = "图片已放进剪贴板，Ctrl+V 直接粘贴";
                    clipsync::write_own(|| clipboard::set_image(&rgba, png.as_deref()))
                }
                None => {
                    clip = "已放进剪贴板，Ctrl+V 粘贴文件";
                    clipsync::write_own(|| clipboard::set_files(&paths))
                }
            },
            _ => {
                clip = "已放进剪贴板，Ctrl+V 粘贴文件";
                clipsync::write_own(|| clipboard::set_files(&paths))
            }
        };
        match seq {
            Some(_) => {}
            None => {
                clip = "";
                log!("⚠ 收到的文件没放进剪贴板：剪贴板被别的程序占着");
            }
        }
    }
    let first = paths[0].file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let title = if paths.len() == 1 { format!("收到手机发来的 {first}") } else { format!("收到手机发来的 {} 个文件", paths.len()) };
    let mut text = format!("存在 {}", paths[0].parent().map(|p| p.display().to_string()).unwrap_or_default());
    if !clip.is_empty() {
        text = format!("{clip}。{text}");
    }
    if failed > 0 {
        text.push_str(&format!("；另有 {failed} 个没收成"));
    }
    tray::balloon_file(&title, &text, paths[0].clone());
}

fn is_image(path: &Path) -> bool {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "bmp" | "webp")
}

/// 解码成 RGBA（按 EXIF 里的方向转正：手机竖着拍的照片多半存的是横的）；原文件是 PNG 的顺便带上原字节。
/// 太大（超过 4000 万像素）的不放图片，放文件
fn load_image(path: &Path) -> Option<(image::RgbaImage, Option<Vec<u8>>)> {
    use image::ImageDecoder;
    let bytes = std::fs::read(path).ok()?;
    let reader = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().ok()?;
    let png = reader.format() == Some(image::ImageFormat::Png);
    let mut decoder = reader.into_decoder().ok()?;
    let (w, h) = decoder.dimensions();
    if w as u64 * h as u64 > 40_000_000 {
        return None;
    }
    let orientation = decoder.orientation().ok();
    let mut image = image::DynamicImage::from_decoder(decoder).ok()?;
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    Some((image.to_rgba8(), if png { Some(bytes) } else { None }))
}

/// 在资源管理器里打开所在文件夹并选中它（点了托盘通知）
pub fn reveal(file: &Path) {
    let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{}", file.display())).spawn();
}

/// 存文件的文件夹（设置里没改就是「下载\蒜鸟」）
pub fn folder() -> PathBuf {
    state::config().settings.receive.folder_in(&downloads())
}

/// 系统里「下载」文件夹的位置（可能被挪到了别的盘，比如 E:\Downloads）
fn downloads() -> PathBuf {
    unsafe {
        if let Ok(path) = SHGetKnownFolderPath(&FOLDERID_Downloads, KNOWN_FOLDER_FLAG(0), None) {
            let text = path.to_string().unwrap_or_default();
            CoTaskMemFree(Some(path.0 as *const _));
            if !text.is_empty() {
                return PathBuf::from(text);
            }
        }
    }
    PathBuf::from(std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into())).join("Downloads")
}

fn free_space(dir: &Path) -> Option<u64> {
    let mut free = 0u64;
    let wide = HSTRING::from(dir.as_os_str());
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut free), None, None) }.ok()?;
    Some(free)
}

/// 手机发来的文件名变成能用的：去掉路径、Windows 不让用的字符和保留名，太长的截短（留着扩展名）
fn clean_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base.chars().map(|c| if c < ' ' || "<>:\"|?*".contains(c) { '_' } else { c }).collect();
    let mut name = cleaned.trim().trim_end_matches(['.', ' ']).to_string();
    if name.is_empty() {
        name = "手机发来的文件".into();
    }
    let stem = name.split('.').next().unwrap_or("").trim().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        name = format!("_{name}");
    }
    let (stem, ext) = split_ext(&name);
    if stem.chars().count() > 100 {
        let short: String = stem.chars().take(100).collect();
        name = format!("{short}{ext}");
    }
    name
}

/// 「照片.jpg」→（「照片」，「.jpg」）；没有扩展名、以点开头的整个当名字
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 && name.len() - i <= 10 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// 文件夹里还没有的名字：重名的加「 (1)」「 (2)」……
fn unique(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = split_ext(name);
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap()
}

/// 1536 → 「1.5 KB」
fn human(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} 字节"),
        b if b < 1 << 20 => format!("{:.0} KB", b as f64 / 1024.0),
        b if b < 1 << 30 => format!("{:.1} MB", b as f64 / 1048576.0),
        b => format!("{:.2} GB", b as f64 / 1073741824.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_made_safe() {
        assert_eq!(clean_name("IMG_001.jpg"), "IMG_001.jpg");
        assert_eq!(clean_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_name("C:\\Windows\\x.dll"), "x.dll");
        assert_eq!(clean_name("a<b>c:d?.txt"), "a_b_c_d_.txt");
        assert_eq!(clean_name("con.txt"), "_con.txt");
        assert_eq!(clean_name("COM1"), "_COM1");
        assert_eq!(clean_name("  ...  "), "手机发来的文件");
        assert_eq!(clean_name("报告. "), "报告");
        let long = format!("{}.pdf", "长".repeat(300));
        assert_eq!(clean_name(&long), format!("{}.pdf", "长".repeat(100)));
    }

    #[test]
    fn duplicate_names_get_numbers() {
        let dir = std::env::temp_dir().join(format!("sn-filerecv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique(&dir, "a.jpg"), dir.join("a.jpg"));
        std::fs::write(dir.join("a.jpg"), b"x").unwrap();
        std::fs::write(dir.join("a (1).jpg"), b"x").unwrap();
        assert_eq!(unique(&dir, "a.jpg"), dir.join("a (2).jpg"));
        std::fs::write(dir.join("README"), b"x").unwrap();
        assert_eq!(unique(&dir, "README"), dir.join("README (1)"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
