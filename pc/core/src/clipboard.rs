//! 剪贴板：用「剪贴板粘贴」输入文字时临时借用剪贴板，粘完把用户原来的内容原样放回去。
//!
//! - 借之前存下剪贴板里的全部格式：文字、HTML、RTF、图片（DIB、DIBV5、PNG）、复制的文件（CF_HDROP）、
//!   各个程序自己注册的格式……这些都是内存块，按字节存；增强图元文件按内容复制。
//!   位图、调色板、旧式图元文件这几种 GDI 句柄不存：放回 DIB、增强图元文件以后系统会重新合成。
//!   OLE 的「DataObject」也不存：它指向原来那个程序的数据对象，那个程序不再占着剪贴板以后就作废了。
//! - 有存不下来的内容（程序私有格式、别的 GDI 句柄、加起来超过 256 MB）就不借：这次改成逐字输入，剪贴板一点不动。
//! - 临时写进去的文字带上「别进剪贴板历史（Win+V）、别上传云剪贴板、剪贴板监视程序别管」三个标记；
//!   放回原内容时也带上，剪贴板历史里不会多出东西。
//! - 放回之前看剪贴板序号：我们写入以后剪贴板又变了（多半是用户自己复制了东西），就不放回，免得盖掉新内容。
//! - 连着粘贴好几次（边说边打字）时，只在第一次存原内容，最后一次粘完过一会儿再放回。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows::core::w;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::Graphics::Gdi::{DeleteEnhMetaFile, GetEnhMetaFileBits, SetEnhMetaFileBits, HENHMETAFILE};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardFormatNameW,
    GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{
    CF_BITMAP, CF_DIB, CF_DIBV5, CF_DSPBITMAP, CF_DSPENHMETAFILE, CF_DSPMETAFILEPICT, CF_ENHMETAFILE, CF_GDIOBJLAST,
    CF_METAFILEPICT, CF_OWNERDISPLAY, CF_PALETTE, CF_PRIVATEFIRST, CF_UNICODETEXT, CLIPBOARD_FORMAT,
};

/// 粘完多久放回原来的内容：目标程序要先把字读走
pub const RESTORE_AFTER: Duration = Duration::from_millis(1000);
/// 剪贴板里的东西加起来超过这么大就不借了（改成逐字输入）
const MAX_BYTES: usize = 256 << 20;

const fn cf(format: CLIPBOARD_FORMAT) -> u32 {
    format.0 as u32
}

enum Data {
    /// 内存块（GlobalAlloc）的全部字节
    Bytes(Vec<u8>),
    /// 增强图元文件的内容（GetEnhMetaFileBits）
    EnhMetaFile(Vec<u8>),
}

impl Data {
    fn len(&self) -> usize {
        match self {
            Data::Bytes(bytes) | Data::EnhMetaFile(bytes) => bytes.len(),
        }
    }
}

struct Item {
    format: u32,
    data: Data,
}

/// 剪贴板里原来的全部内容（按原来的顺序）
pub struct Saved {
    items: Vec<Item>,
}

/// 打开着的剪贴板，离开作用域时关上
struct Opened;

impl Opened {
    /// 别的程序可能正占着剪贴板：稍等重试，最多半秒
    fn new() -> Option<Self> {
        for _ in 0..25 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Some(Self);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

/// 「剪贴板监视程序别管」「别进剪贴板历史」「别上传云剪贴板」三个标记格式
fn markers() -> &'static [u32; 3] {
    static MARKERS: OnceLock<[u32; 3]> = OnceLock::new();
    MARKERS.get_or_init(|| unsafe {
        [
            RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")),
            RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")),
            RegisterClipboardFormatW(w!("CanUploadToCloudClipboard")),
        ]
    })
}

/// OLE 放在剪贴板上的「DataObject」：指向原程序的数据对象，原程序不再占着剪贴板以后就作废了
fn data_object_format() -> u32 {
    static FORMAT: OnceLock<u32> = OnceLock::new();
    *FORMAT.get_or_init(|| unsafe { RegisterClipboardFormatW(w!("DataObject")) })
}

/// 剪贴板序号：剪贴板每变一次加一
pub fn sequence() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

fn format_name(format: u32) -> String {
    let mut name = [0u16; 128];
    let len = unsafe { GetClipboardFormatNameW(format, &mut name) };
    if len > 0 {
        String::from_utf16_lossy(&name[..len as usize])
    } else {
        format!("格式 {format}")
    }
}

/// 剪贴板里某个内存块格式的全部字节（剪贴板要已经打开）
unsafe fn global_bytes(format: u32) -> Option<Vec<u8>> {
    let handle = unsafe { GetClipboardData(format) }.ok()?;
    let global = HGLOBAL(handle.0);
    let size = unsafe { GlobalSize(global) };
    if size == 0 {
        return None;
    }
    let source = unsafe { GlobalLock(global) } as *const u8;
    if source.is_null() {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(source, size) }.to_vec();
    unsafe {
        let _ = GlobalUnlock(global);
    }
    Some(bytes)
}

/// 剪贴板里的增强图元文件的内容（剪贴板要已经打开）
unsafe fn enh_metafile_bits() -> Option<Vec<u8>> {
    let handle = unsafe { GetClipboardData(cf(CF_ENHMETAFILE)) }.ok()?;
    let metafile = HENHMETAFILE(handle.0);
    let size = unsafe { GetEnhMetaFileBits(metafile, None) };
    if size == 0 {
        return None;
    }
    let mut bits = vec![0u8; size as usize];
    (unsafe { GetEnhMetaFileBits(metafile, Some(&mut bits)) } == size).then_some(bits)
}

/// 把字节放进剪贴板（剪贴板要已经打开、清空）
unsafe fn put_bytes(format: u32, bytes: &[u8]) -> bool {
    let Ok(global) = (unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) }) else { return false };
    let target = unsafe { GlobalLock(global) } as *mut u8;
    if target.is_null() {
        unsafe {
            let _ = GlobalFree(Some(global));
        }
        return false;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
        let _ = GlobalUnlock(global);
    }
    // 放进去以后这块内存归系统管；没放进去才自己释放
    if unsafe { SetClipboardData(format, Some(HANDLE(global.0))) }.is_ok() {
        return true;
    }
    unsafe {
        let _ = GlobalFree(Some(global));
    }
    false
}

unsafe fn put_enh_metafile(bits: &[u8]) -> bool {
    let metafile = unsafe { SetEnhMetaFileBits(bits) };
    if metafile.is_invalid() {
        return false;
    }
    if unsafe { SetClipboardData(cf(CF_ENHMETAFILE), Some(HANDLE(metafile.0))) }.is_ok() {
        return true;
    }
    unsafe {
        let _ = DeleteEnhMetaFile(Some(metafile));
    }
    false
}

/// 三个标记都写 0：别进历史、别上传（第一个标记只看有没有）
unsafe fn put_markers() {
    for &format in markers() {
        unsafe { put_bytes(format, &0u32.to_le_bytes()) };
    }
}

impl Saved {
    /// 存下剪贴板里现在的全部内容。有存不下来的就返回原因：这次不借剪贴板
    pub fn take() -> Result<Saved, String> {
        let _open = Opened::new().ok_or_else(|| "剪贴板被别的程序占着".to_string())?;
        let mut items = Vec::new();
        let mut total = 0;
        let (mut bitmap, mut metafile) = (false, false);
        let mut format = 0;
        loop {
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            let data = match format {
                // GDI 句柄：放回 DIB、增强图元文件以后系统会重新合成
                f if f == cf(CF_BITMAP) => {
                    bitmap = true;
                    continue;
                }
                f if f == cf(CF_METAFILEPICT) => {
                    metafile = true;
                    continue;
                }
                f if f == cf(CF_PALETTE) || f == cf(CF_OWNERDISPLAY) || f == data_object_format() => continue,
                f if f == cf(CF_ENHMETAFILE) => unsafe { enh_metafile_bits() }.map(Data::EnhMetaFile),
                // 程序私有格式、别的 GDI 句柄：只有原来的程序知道怎么用，复制不了
                f if f == cf(CF_DSPBITMAP)
                    || f == cf(CF_DSPMETAFILEPICT)
                    || f == cf(CF_DSPENHMETAFILE)
                    || (cf(CF_PRIVATEFIRST)..=cf(CF_GDIOBJLAST)).contains(&f) =>
                {
                    return Err(format!("存不下来的内容（{}）", format_name(f)));
                }
                f => unsafe { global_bytes(f) }.map(Data::Bytes),
            };
            // 拿不到的（原来的程序已经给不出来了）谁也粘贴不了，不用存
            let Some(data) = data else { continue };
            total += data.len();
            if total > MAX_BYTES {
                return Err(format!("超过 {} MB 的内容", MAX_BYTES >> 20));
            }
            items.push(Item { format, data });
        }
        // 系统一般会把能合成的 DIB、增强图元文件也列出来（上面已经存了），没列出来的话这里补上
        if bitmap && !items.iter().any(|i| i.format == cf(CF_DIB) || i.format == cf(CF_DIBV5)) {
            let bytes = unsafe { global_bytes(cf(CF_DIB)) }.ok_or("存不下来的图片")?;
            items.push(Item { format: cf(CF_DIB), data: Data::Bytes(bytes) });
        }
        if metafile && !items.iter().any(|i| i.format == cf(CF_ENHMETAFILE)) {
            let bits = unsafe { enh_metafile_bits() }.ok_or("存不下来的图元文件")?;
            items.push(Item { format: cf(CF_ENHMETAFILE), data: Data::EnhMetaFile(bits) });
        }
        Ok(Saved { items })
    }

    /// 清空剪贴板，按原来的顺序放回去，再带上三个标记。剪贴板打不开时返回 None，否则返回放回了几个、一共几个
    fn put_back(&self) -> Option<(usize, usize)> {
        let _open = Opened::new()?;
        unsafe {
            EmptyClipboard().ok()?;
        }
        // 原来就是空的：清空就行，不写标记
        if self.items.is_empty() {
            return Some((0, 0));
        }
        let mut done = 0;
        let mut total = 0;
        for item in self.items.iter().filter(|i| !markers().contains(&i.format)) {
            total += 1;
            let ok = match &item.data {
                Data::Bytes(bytes) => unsafe { put_bytes(item.format, bytes) },
                Data::EnhMetaFile(bits) => unsafe { put_enh_metafile(bits) },
            };
            done += ok as usize;
        }
        unsafe { put_markers() };
        Some((done, total))
    }
}

/// 把文字临时放进剪贴板（带上三个标记）。成功返回写完以后的剪贴板序号；
/// 失败时 Err(true) 表示剪贴板已经清空了、要马上放回原来的，Err(false) 表示剪贴板没动过
fn put_text(text: &str) -> Result<u32, bool> {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };
    let Some(open) = Opened::new() else { return Err(false) };
    unsafe {
        if EmptyClipboard().is_err() {
            return Err(false);
        }
        if !put_bytes(cf(CF_UNICODETEXT), bytes) {
            return Err(true);
        }
        put_markers();
    }
    // 序号要在关上以后取：关上剪贴板时序号还会变
    drop(open);
    Ok(sequence())
}

/// 借着剪贴板、还没还的：原来的内容，我们写进去以后的剪贴板序号，第几次借
struct Borrowed {
    saved: Saved,
    seq: u32,
    id: u64,
}

static BORROWED: Mutex<Option<Borrowed>> = Mutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// 借剪贴板粘贴：放进文字、调 paste（按 Ctrl+V）、过 restore_after 把原来的内容放回去。
/// 借不了（有存不下来的内容、剪贴板被占着）返回 false，剪贴板没动过，由调用的地方改成逐字输入
pub fn paste_with(text: &str, restore_after: Duration, paste: impl FnOnce()) -> bool {
    let mut borrowed = BORROWED.lock().unwrap();
    // 上次借的还没还、剪贴板里还是我们写的字：原来的内容就是上次存的，别把我们自己的字当成原内容
    let still_ours = borrowed.as_ref().is_some_and(|b| b.seq == sequence());
    let fresh = if still_ours {
        None
    } else {
        // 上次借了以后剪贴板变过（用户复制了别的）：上次存的不该再放回去了
        *borrowed = None;
        match Saved::take() {
            Ok(saved) => Some(saved),
            Err(why) => {
                crate::log!("📋 剪贴板里有{why}，这次改成逐字输入，不动剪贴板");
                return false;
            }
        }
    };
    let seq = match put_text(text) {
        Ok(seq) => seq,
        Err(emptied) => {
            if emptied {
                // 已经清空了但文字没写进去：马上把原来的放回去
                if let Some(saved) = fresh.as_ref().or(borrowed.as_ref().map(|b| &b.saved)) {
                    saved.put_back();
                }
                *borrowed = None;
            }
            crate::log!("📋 剪贴板写不进去，这次改成逐字输入");
            return false;
        }
    };
    let saved = match fresh {
        Some(saved) => saved,
        None => borrowed.take().expect("上次借的").saved,
    };
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    *borrowed = Some(Borrowed { saved, seq, id });
    drop(borrowed);
    paste();
    std::thread::Builder::new()
        .name("clipboard-restore".into())
        .spawn(move || {
            std::thread::sleep(restore_after);
            give_back(id);
        })
        .ok();
    true
}

/// 后台要退出了：还借着剪贴板的话现在就还（剪贴板没被别人改过才还），不然原来的内容就丢了
pub fn give_back_now() {
    let id = BORROWED.lock().unwrap().as_ref().map(|b| b.id);
    if let Some(id) = id {
        give_back(id);
    }
}

/// 粘完过了一会儿：剪贴板里还是我们写的字，就把原来的内容放回去
fn give_back(id: u64) {
    for _ in 0..5 {
        {
            let mut borrowed = BORROWED.lock().unwrap();
            let Some(b) = borrowed.as_ref().filter(|b| b.id == id) else {
                return; // 之后又粘贴过，由最后那次放回
            };
            if sequence() != b.seq {
                crate::log!("📋 粘贴以后剪贴板又变了（多半是你自己复制了东西），原来的内容不放回去");
                *borrowed = None;
                return;
            }
            if let Some((done, total)) = b.saved.put_back() {
                if done < total {
                    crate::log!("⚠ 剪贴板原来的内容放回了 {done}/{total} 种格式");
                }
                *borrowed = None;
                return;
            }
        }
        // 剪贴板被别的程序占着：等一会儿再试（不占着锁，别耽误下一次粘贴）
        std::thread::sleep(Duration::from_millis(300));
    }
    let mut borrowed = BORROWED.lock().unwrap();
    if borrowed.as_ref().is_some_and(|b| b.id == id) {
        *borrowed = None;
        crate::log!("⚠ 剪贴板一直被别的程序占着，原来的内容没能放回去");
    }
}

#[cfg(test)]
mod tests {
    //! 这些测试在自己建的窗口站里跑：那里的剪贴板和用户的剪贴板是分开的，跑测试不会动到用户的剪贴板。
    //! paste_into_foreground_window 例外：它用真剪贴板、真按 Ctrl+V，只在手动验证时跑（见函数说明）。

    use super::*;
    use windows::core::HSTRING;
    use windows::Win32::Graphics::Gdi::{CloseEnhMetaFile, CreateEnhMetaFileW, Rectangle};
    use windows::Win32::System::DataExchange::CountClipboardFormats;
    use windows::Win32::System::Ole::{CF_HDROP, CF_LOCALE, CF_OEMTEXT, CF_TEXT};
    use windows::Win32::System::StationsAndDesktops::{CreateWindowStationW, SetProcessWindowStation};

    /// 剪贴板是整个窗口站共用的：测试一个一个来
    static SERIAL: Mutex<()> = Mutex::new(());

    fn isolate() -> std::sync::MutexGuard<'static, ()> {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| unsafe {
            let user_seq = sequence();
            let name = HSTRING::from(format!("PhonePadClipboardTest{}", std::process::id()));
            let station = CreateWindowStationW(&name, 0, 0x37F, None).expect("建测试窗口站");
            SetProcessWindowStation(station).expect("换到测试窗口站");
            // 确认真的换过来了（新窗口站的剪贴板是空的、序号从头算），不然一个格式都不写
            assert!(sequence() != user_seq && CountClipboardFormats() == 0, "没能和用户的剪贴板隔开");
        });
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn registered(name: &str) -> u32 {
        unsafe { RegisterClipboardFormatW(&HSTRING::from(name)) }
    }

    /// 剪贴板里全部格式：(格式, 内容)，GDI 句柄的内容是 None
    fn dump() -> Vec<(u32, Option<Vec<u8>>)> {
        let _open = Opened::new().expect("打开剪贴板");
        let mut out = Vec::new();
        let mut format = 0;
        loop {
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            let data = if format == cf(CF_ENHMETAFILE) {
                unsafe { enh_metafile_bits() }
            } else if [cf(CF_BITMAP), cf(CF_METAFILEPICT), cf(CF_PALETTE)].contains(&format) {
                None
            } else {
                unsafe { global_bytes(format) }
            };
            out.push((format, data));
        }
        out
    }

    /// 像别的程序那样往剪贴板里放东西（不带标记）
    fn put(items: &[(u32, Vec<u8>)], metafile: bool) {
        let _open = Opened::new().expect("打开剪贴板");
        unsafe {
            EmptyClipboard().unwrap();
            for (format, bytes) in items {
                assert!(put_bytes(*format, bytes));
            }
            if metafile {
                let dc = CreateEnhMetaFileW(None, None, None, None);
                let _ = Rectangle(dc, 10, 10, 200, 120);
                let emf = CloseEnhMetaFile(dc);
                assert!(!emf.is_invalid());
                SetClipboardData(cf(CF_ENHMETAFILE), Some(HANDLE(emf.0))).unwrap();
            }
        }
    }

    fn utf16z(text: &str) -> Vec<u8> {
        text.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect()
    }

    fn text_now() -> Option<String> {
        let _open = Opened::new().expect("打开剪贴板");
        let bytes = unsafe { global_bytes(cf(CF_UNICODETEXT)) }?;
        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
        Some(String::from_utf16_lossy(&units))
    }

    /// 3×2 的 24 位 DIB（BITMAPINFOHEADER + 每行补齐到 4 字节）
    fn dib() -> Vec<u8> {
        let mut bytes = Vec::new();
        for v in [40u32, 3, 2] {
            bytes.extend(v.to_le_bytes());
        }
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(24u16.to_le_bytes());
        bytes.extend([0u8; 24]);
        for row in 0..2u8 {
            bytes.extend([row, 0x40, 0x80, 0xC0, row, 0x10, 0x20, 0x30, row, 0, 0, 0]);
        }
        bytes
    }

    /// 复制了两个文件时的 CF_HDROP：DROPFILES 头（宽字符）+ 两个路径 + 两个 0
    fn hdrop() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(20u32.to_le_bytes());
        bytes.extend([0u8; 12]);
        bytes.extend(1u32.to_le_bytes());
        for path in [r"C:\测试\图片.png", r"D:\文档\笔记.txt"] {
            bytes.extend(utf16z(path));
        }
        bytes.extend([0u8; 2]);
        bytes
    }

    fn dword(items: &[(u32, Option<Vec<u8>>)], format: u32) -> Option<u32> {
        items.iter().find(|(f, _)| *f == format).and_then(|(_, d)| d.as_deref()).map(|d| u32::from_le_bytes(d[..4].try_into().unwrap()))
    }

    #[test]
    fn reading_does_not_change_sequence() {
        let _serial = isolate();
        put(&[(cf(CF_UNICODETEXT), utf16z("原来的"))], false);
        let seq = put_text("临时的字").unwrap();
        assert_eq!(seq, sequence(), "写完关上剪贴板以后序号就不变了");
        // 目标程序读文字，包括系统合成的 ANSI 文字、区域：不算改了剪贴板
        {
            let _open = Opened::new().unwrap();
            for format in [cf(CF_UNICODETEXT), cf(CF_TEXT), cf(CF_OEMTEXT), cf(CF_LOCALE)] {
                assert!(unsafe { GetClipboardData(format) }.is_ok());
            }
        }
        assert_eq!(seq, sequence());
        put(&[(cf(CF_UNICODETEXT), utf16z("用户新复制的"))], false);
        assert_ne!(seq, sequence());
    }

    #[test]
    fn everything_comes_back() {
        let _serial = isolate();
        let html = b"Version:0.9\r\nStartHTML:0000000105\r\nEndHTML:0000000163\r\n<html><body><b>\xe5\x9b\xbe</b></body></html>".to_vec();
        let custom = (0..=255u8).cycle().take(70_000).collect::<Vec<u8>>();
        let original = [
            (registered("PNG"), b"\x89PNG\r\n\x1a\n fake png bytes".to_vec()),
            (cf(CF_DIB), dib()),
            (registered("HTML Format"), html),
            (registered("Rich Text Format"), b"{\\rtf1 hello}".to_vec()),
            (cf(CF_HDROP), hdrop()),
            (registered("PhonePad Test Format"), custom),
            (cf(CF_UNICODETEXT), utf16z("图片说明")),
        ];
        put(&original, true);
        let before = dump();
        let mut during = Vec::new();
        assert!(paste_with("临时写进去的字", Duration::from_millis(100), || during = dump()));
        // 粘贴的时候：只有我们的字，加三个标记（都是 0）
        assert_eq!(during.iter().find(|(f, _)| *f == cf(CF_UNICODETEXT)).unwrap().1.as_deref(), Some(&utf16z("临时写进去的字")[..]));
        for &marker in markers() {
            assert_eq!(dword(&during, marker), Some(0));
        }
        assert!(!during.iter().any(|(f, _)| *f == registered("PNG") || *f == cf(CF_DIB)));
        std::thread::sleep(Duration::from_millis(400));
        let after = dump();
        // 原来每个格式（包括系统合成出来的）都回来了，内容一字不差，顺序不变
        let mut last = 0;
        for (format, data) in &before {
            let position = after.iter().position(|(f, _)| f == format).unwrap_or_else(|| panic!("{} 没回来", format_name(*format)));
            if data.is_some() {
                assert_eq!(&after[position].1, data, "{} 内容变了", format_name(*format));
            }
            if original.iter().any(|(f, _)| f == format) {
                assert!(position >= last, "{} 顺序变了", format_name(*format));
                last = position;
            }
        }
        assert!(after.iter().any(|(f, d)| *f == cf(CF_ENHMETAFILE) && d.is_some()));
        assert!(after.iter().any(|(f, _)| *f == cf(CF_BITMAP)), "位图由 DIB 合成");
        for &marker in markers() {
            assert_eq!(dword(&after, marker), Some(0));
        }
    }

    #[test]
    fn user_copy_is_kept() {
        let _serial = isolate();
        put(&[(cf(CF_UNICODETEXT), utf16z("原来的")), (registered("PNG"), b"png".to_vec())], false);
        assert!(paste_with("临时的字", Duration::from_millis(200), || {}));
        std::thread::sleep(Duration::from_millis(50));
        put(&[(cf(CF_UNICODETEXT), utf16z("用户新复制的"))], false);
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(text_now().as_deref(), Some("用户新复制的"));
        assert!(BORROWED.lock().unwrap().is_none());
    }

    #[test]
    fn back_to_back_pastes_keep_the_original() {
        let _serial = isolate();
        put(&[(registered("PNG"), b"original image".to_vec())], false);
        assert!(paste_with("第一段", Duration::from_millis(300), || {}));
        std::thread::sleep(Duration::from_millis(100));
        assert!(paste_with("第二段", Duration::from_millis(300), || {}));
        // 第一次的放回时间到了：后来又粘贴过，不放回
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(text_now().as_deref(), Some("第二段"));
        std::thread::sleep(Duration::from_millis(300));
        let after = dump();
        assert_eq!(after.iter().find(|(f, _)| *f == registered("PNG")).and_then(|(_, d)| d.as_deref()), Some(&b"original image"[..]));
        assert!(text_now().is_none(), "原来没有文字");
    }

    #[test]
    fn exit_gives_back_at_once() {
        let _serial = isolate();
        put(&[(registered("PNG"), b"original image".to_vec())], false);
        assert!(paste_with("临时的字", Duration::from_secs(30), || {}));
        give_back_now();
        assert!(BORROWED.lock().unwrap().is_none());
        let after = dump();
        assert_eq!(after.iter().find(|(f, _)| *f == registered("PNG")).and_then(|(_, d)| d.as_deref()), Some(&b"original image"[..]));
    }

    #[test]
    fn empty_clipboard_stays_empty() {
        let _serial = isolate();
        put(&[], false);
        assert!(paste_with("临时的字", Duration::from_millis(50), || {}));
        std::thread::sleep(Duration::from_millis(300));
        assert!(dump().is_empty());
    }

    #[test]
    fn private_format_means_no_borrowing() {
        let _serial = isolate();
        put(&[(cf(CF_UNICODETEXT), utf16z("原来的"))], false);
        {
            // 程序私有格式：句柄只有原程序知道是什么，系统也不替它释放
            let _open = Opened::new().unwrap();
            unsafe {
                let global = GlobalAlloc(GMEM_MOVEABLE, 8).unwrap();
                SetClipboardData(cf(CF_PRIVATEFIRST), Some(HANDLE(global.0))).unwrap();
            }
        }
        let seq = sequence();
        assert!(!paste_with("临时的字", Duration::from_millis(50), || panic!("不该按 Ctrl+V")));
        assert_eq!(seq, sequence(), "剪贴板没动过");
        assert_eq!(text_now().as_deref(), Some("原来的"));
    }

    /// 手动验证用：真剪贴板、真按 Ctrl+V，粘贴到 PP_TEST_HWND 这个窗口（必须正在前台，不是就不粘贴）。
    /// cargo test -p phonepad-core paste_into_foreground_window -- --ignored --exact
    #[test]
    #[ignore]
    fn paste_into_foreground_window() {
        use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        let hwnd: isize = std::env::var("PP_TEST_HWND").expect("PP_TEST_HWND").parse().unwrap();
        let text = std::env::var("PP_TEST_TEXT").unwrap_or_else(|_| "PhonePad 粘贴测试".into());
        let foreground = unsafe { GetForegroundWindow() };
        assert_eq!(foreground.0 as isize, hwnd, "前台不是测试窗口，不粘贴");
        crate::input::paste_text(&text);
        println!("pasted at seq {}", sequence());
        std::thread::sleep(RESTORE_AFTER + Duration::from_millis(1500));
        println!("done at seq {}", sequence());
    }
}
