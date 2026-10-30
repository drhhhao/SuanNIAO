//! 剪贴板同步（设置里「和手机同步剪贴板」开着时）：
//! - 手机 → 电脑：手机上只有蒜鸟在前台时才读得到剪贴板，所以手机回到蒜鸟时看剪贴板变了没有，变了就把文字发过来（clip），
//!   这边放进剪贴板。在手机上复制、切回蒜鸟、按粘贴键（Ctrl+V），粘的就是手机上复制的。
//! - 电脑 → 手机：剪贴板一变（WM_CLIPBOARDUPDATE）、里面是文字，就发给手机（clip），手机放进自己的剪贴板。
//!   没有文字、是图片（截图、复制的图片）时编成 PNG / JPEG（clipimage），分块发：clipImg（id、mime、size、w、h）、
//!   一串 clipImgPart（id、d＝Base64）、clipImgEnd（id）；手机收齐了放进剪贴板。传的时候剪贴板又变了就发 clipImgCancel 不传了。
//!   图片只从电脑到手机：手机上很多 App 复制图片不进系统剪贴板，反过来用处不大。
//! - 不发：设置里关着、没连手机、带「别管 / 别进历史 / 别上传」标记的（密码管理器复制的密码、借剪贴板输入语音文字时写的）、
//!   和上一次同步的一样、超过 MAX_BYTES 的。手机发来的写进剪贴板以后记下序号，因此变的那一次不发回去。

use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use base64::Engine;

use serde_json::json;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::AddClipboardFormatListener;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::util::preview;
use crate::clipboard::Shared;
use crate::{clipboard, clipimage, link, log, state};

/// 一次最多同步这么多（UTF-8 字节）：再长的多半不是要粘的字，蓝牙上也要传好一会儿
pub const MAX_BYTES: usize = 256 << 10;

/// 自己写进剪贴板（手机发来的文字、手机发来的文件）以后的序号
static OWN_SEQ: AtomicU32 = AtomicU32::new(0);
/// 正在往剪贴板里写自己的东西：剪贴板通知可能比写完、记下序号来得早，这期间的变化也不发回手机
static WRITING: AtomicBool = AtomicBool::new(false);

/// 往剪贴板里写自己的东西（write 返回写完以后的序号）：这次变化不发回手机
pub fn write_own(write: impl FnOnce() -> Option<u32>) -> Option<u32> {
    WRITING.store(true, Ordering::SeqCst);
    let seq = write();
    if let Some(seq) = seq {
        OWN_SEQ.store(seq, Ordering::SeqCst);
    }
    WRITING.store(false, Ordering::SeqCst);
    seq
}

/// 剪贴板这次的变化是自己写的
fn own_change() -> bool {
    WRITING.load(Ordering::SeqCst) || clipboard::sequence() == OWN_SEQ.load(Ordering::SeqCst)
}
/// 上一次同步的文字（发出去的或者收到的）
static LAST: Mutex<String> = Mutex::new(String::new());
/// 图片编好以后最多这么大，再大就编成 JPEG、缩小：小米 13 实测蓝牙上约 55 KB/s，512 KB 要传 9 秒左右
pub const MAX_IMAGE_BYTES: usize = 512 << 10;
/// 每块原始字节数（Base64 以后约 21 KB 一行）：一块一块地发，传图片时别的消息（心跳回复、配置）也插得进去。
/// 实测 8 KB、16 KB 差不多（49～59 KB/s），48 KB 反而慢（33～42 KB/s）
const IMAGE_CHUNK: usize = 16 << 10;
/// 要同步新内容（新的文字、另一张图片）时加一：正在传的图片发现变了就不传了
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// 上一次发出去的图片（原始剪贴板内容的指纹），同一张不重发
static LAST_IMAGE: AtomicU64 = AtomicU64::new(0);
static NEXT_IMAGE_ID: AtomicU64 = AtomicU64::new(1);

fn enabled() -> bool {
    state::config().settings.clipboard_sync
}

/// 建一个只收消息的窗口，登记剪贴板监听，跑消息循环（单独一个线程，跟着后台退出）
pub fn run() {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("SuanNIAOClipboardSync");
        let wc = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: instance.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("SuanNIAOClipboardSync"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        ) else {
            log!("❌ 剪贴板同步开不起来（建窗口失败）");
            return;
        };
        if AddClipboardFormatListener(hwnd).is_err() {
            log!("❌ 剪贴板同步开不起来（登记不了剪贴板监听）");
            return;
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_CLIPBOARDUPDATE {
        on_update();
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// 电脑上剪贴板变了：能同步的文字、图片发给手机
fn on_update() {
    if own_change() || !enabled() || !state::status().connected {
        return;
    }
    match clipboard::shareable() {
        Some(Shared::Text(text)) => send_text(text),
        Some(image) => start_image(image),
        // 刚复制完图片的那一下常常还取不到位图（放图片的程序还在准备，比如 .NET 程序的 OleFlushClipboard），
        // 过几十毫秒就好了：等一等再读。读到的已经不是图片（又复制了别的）就算了
        None if clipboard::has_image() => {
            std::thread::spawn(|| {
                for ms in [50, 150, 400, 1000] {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                    match clipboard::shareable() {
                        Some(Shared::Text(_)) => return,
                        Some(image) => return start_image(image),
                        None if !clipboard::has_image() => return,
                        None => {}
                    }
                }
                log!("🖼 电脑上复制的图片读不出来，没同步到手机");
            });
        }
        None => {}
    }
}

fn send_text(text: String) {
    if text.trim().is_empty() {
        return;
    }
    if text.len() > MAX_BYTES {
        log!("📋 电脑上复制的文字太长（{} KB），没同步到手机", text.len() >> 10);
        return;
    }
    {
        let mut last = LAST.lock().unwrap();
        if *last == text {
            return;
        }
        *last = text.clone();
    }
    // 正在传的图片不传了；以后再复制同一张图片照样发
    GENERATION.fetch_add(1, Ordering::Relaxed);
    LAST_IMAGE.store(0, Ordering::Relaxed);
    if link::send_to_phone(&json!({"t": "clip", "s": text})) {
        log!("📋 电脑上复制的 {} 字已同步到手机：{}", text.chars().count(), preview(&text));
    }
}

/// 新的图片：同一张（一次复制常常来两三次剪贴板通知）不重发，也不打断正在传的
fn start_image(image: Shared) {
    if own_change() {
        return; // 等一等再读的那条路：读到的是自己刚写进去的（手机发来的图片）
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match &image {
        Shared::Png(bytes) | Shared::Dib(bytes) => bytes.hash(&mut hasher),
        Shared::Text(_) => return,
    }
    let fingerprint = hasher.finish();
    if LAST_IMAGE.swap(fingerprint, Ordering::Relaxed) == fingerprint {
        return;
    }
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    LAST.lock().unwrap().clear();
    // 编码、传输要好一会儿，别占着收剪贴板通知的线程
    std::thread::spawn(move || send_image(image, generation));
}

fn send_image(image: Shared, generation: u64) {
    let started = Instant::now();
    let encoded = match clipimage::encode(image, MAX_IMAGE_BYTES) {
        Ok(encoded) => encoded,
        Err(why) => {
            log!("🖼 电脑上复制的图片没同步到手机：{why}");
            LAST_IMAGE.store(0, Ordering::Relaxed);
            return;
        }
    };
    let current = || GENERATION.load(Ordering::Relaxed) == generation;
    if !current() {
        return;
    }
    let id = NEXT_IMAGE_ID.fetch_add(1, Ordering::Relaxed);
    let (kind, size) = (if encoded.mime == "image/png" { "PNG" } else { "JPEG" }, encoded.bytes.len());
    let what = format!("{kind} {}×{}，{} KB", encoded.width, encoded.height, size.div_ceil(1024));
    let encode_ms = started.elapsed().as_millis();
    let header = json!({"t": "clipImg", "id": id, "mime": encoded.mime, "size": size, "w": encoded.width, "h": encoded.height});
    if !link::send_to_phone(&header) {
        LAST_IMAGE.store(0, Ordering::Relaxed);
        return;
    }
    let sending = Instant::now();
    for chunk in encoded.bytes.chunks(IMAGE_CHUNK) {
        if !current() {
            link::send_to_phone(&json!({"t": "clipImgCancel", "id": id}));
            log!("🖼 图片（{what}）还没传完剪贴板又变了，不传了");
            return;
        }
        let data = base64::engine::general_purpose::STANDARD.encode(chunk);
        if !link::send_to_phone(&json!({"t": "clipImgPart", "id": id, "d": data})) {
            log!("🖼 图片（{what}）没传完：手机断开了");
            LAST_IMAGE.store(0, Ordering::Relaxed);
            return;
        }
    }
    link::send_to_phone(&json!({"t": "clipImgEnd", "id": id}));
    let secs = sending.elapsed().as_secs_f64();
    log!(
        "🖼 电脑上复制的图片已同步到手机（{what}），编码 {encode_ms} ms，传输 {secs:.1} 秒（约 {:.0} KB/s）",
        size as f64 / 1024.0 / secs.max(0.001)
    );
}

/// 手机发来它剪贴板里的文字：放进电脑的剪贴板
pub fn from_phone(text: &str) {
    if !enabled() {
        log!("📋 手机发来了剪贴板，设置里没开「和手机同步剪贴板」，不放");
        return;
    }
    if text.is_empty() || text.len() > MAX_BYTES {
        return;
    }
    *LAST.lock().unwrap() = text.to_string();
    if clipboard::shareable_text().as_deref() == Some(text) {
        return; // 电脑上已经是这段了
    }
    match write_own(|| clipboard::set_text(text)) {
        Some(_) => {
            log!("📋 手机上复制的 {} 字已放进电脑剪贴板：{}", text.chars().count(), preview(text));
        }
        None => log!("⚠ 手机发来的剪贴板没放进去：剪贴板被别的程序占着"),
    }
}

