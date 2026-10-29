//! 模拟鼠标键盘。指针用 SetCursorPos 按物理像素精确移动，点击、滚轮、按键、文字用 SendInput。
//!
//! 蓝牙送消息时快时慢，直接照着走指针会一顿一跳。所以触控板的移动按手机上手指移动的时间匀速回放
//! （见 playout.rs），由回放线程每 1 ms 走一步。
//!
//! 按键「直通」（手机上按住、电脑上就一直按着）：每个键按引用计数管理，两个手机按键共用同一个修饰键时
//! 最后一个松开才抬起；非修饰键像实体键盘一样 400 ms 后每 80 ms 补发一次按下（修饰键、媒体键不补发，音量加减补发）。
//! 断线、息屏、看门狗超时、改配置、暂停、退出时 release_all 把按着的鼠标键和键盘键都松开。

use std::collections::HashMap;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

use crate::playout::{Playout, Tuning};

/// 写进 dwExtraInfo，标记本程序注入的输入。
const INJECT_TAG: usize = 0x5050_4144;

/// MapVirtualKey 不一定报得出扩展键，这些键要带 EXTENDEDKEY 标志，否则方向键可能被当成小键盘、
/// 媒体键（包括 MediaStop）可能不起作用。
const EXTENDED_VK: [u16; 22] = [
    0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x2D, 0x2E, 0x5B, 0x5C, 0x5D, 0xA3, 0xA5, 0xAD, 0xAE, 0xAF,
    0xB0, 0xB1, 0xB2, 0xB3,
];

/// 不足 1 像素的移动先攒着。
static FRACTION: Mutex<(f64, f64)> = Mutex::new((0.0, 0.0));
/// 收到了但还没放完的移动，由回放线程匀速走掉。
static PLAYOUT: LazyLock<Mutex<Playout>> = LazyLock::new(|| Mutex::new(Playout::new(Tuning::default())));
static WAKE: Condvar = Condvar::new();
static PLAYER: OnceLock<()> = OnceLock::new();
static SMOOTHING: AtomicBool = AtomicBool::new(true);
/// 比屏幕刷新快得多，每帧走的距离才均匀
const PLAY_TICK: Duration = Duration::from_millis(1);
/// 回放用的时钟（ms）
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);
/// 手机上按住还没松开的鼠标键，断线或退出时替它松开。
static HELD: Mutex<Vec<Button>> = Mutex::new(Vec::new());

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Button {
    Left,
    Right,
    Middle,
    /// 侧键：后退（XBUTTON1）
    Back,
    /// 侧键：前进（XBUTTON2）
    Forward,
}

impl Button {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "middle" => Some(Self::Middle),
            "back" => Some(Self::Back),
            "forward" => Some(Self::Forward),
            _ => None,
        }
    }

    /// （按下标志，松开标志，mouseData）
    fn flags(self) -> (MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS, i32) {
        match self {
            Self::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
            Self::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
            Self::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
            Self::Back => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 1),
            Self::Forward => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 2),
        }
    }
}

/// 按物理像素移动指针，否则 150% 缩放下坐标会被系统换算，移动变粗糙。
pub fn enable_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

fn send(inputs: &[INPUT]) {
    if inputs.is_empty() {
        return;
    }
    if crate::trace::dry_run() {
        crate::trace::event("send", &[inputs.len() as f64]);
        return;
    }
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        crate::log!(
            "⚠ 输入被系统拦截（{sent}/{}）：目标窗口可能以管理员身份运行，或电脑在锁屏/UAC 界面",
            inputs.len()
        );
    }
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: INJECT_TAG },
        },
    }
}

fn key_input(vk: u16, up: bool) -> INPUT {
    let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC_EX) };
    let mut flags = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
    if matches!(scan >> 8, 0xE0 | 0xE1) || EXTENDED_VK.contains(&vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: (scan & 0xFF) as u16, dwFlags: flags, time: 0, dwExtraInfo: INJECT_TAG },
        },
    }
}

fn char_input(unit: u16, up: bool) -> INPUT {
    let flags = if up { KEYEVENTF_UNICODE | KEYEVENTF_KEYUP } else { KEYEVENTF_UNICODE };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: unit, dwFlags: flags, time: 0, dwExtraInfo: INJECT_TAG },
        },
    }
}

pub fn set_smoothing(on: bool) {
    SMOOTHING.store(on, Ordering::Relaxed);
    if !on {
        flush_motion();
    }
}

pub fn smoothing() -> bool {
    SMOOTHING.load(Ordering::Relaxed)
}

/// 触控板的移动。ts：手机上这段移动最后一个触摸点的时间（ms），按它匀速回放；旧版手机 App 不带
pub fn move_by(dx: f64, dy: f64, ts: Option<f64>) {
    if !smoothing() {
        return apply_move(dx, dy);
    }
    PLAYER.get_or_init(|| {
        std::thread::Builder::new().name("playout".into()).spawn(play_loop).expect("start playout");
    });
    let mut playout = PLAYOUT.lock().unwrap();
    playout.push(now_ms(), ts, dx, dy);
    crate::trace::event("play", &playout.stats());
    WAKE.notify_one();
}

/// 回放现在比最快送到的那条多等了多少（ms）
pub fn motion_wait() -> f64 {
    PLAYOUT.lock().unwrap().stats()[2]
}

/// 立即移动指针（按键映射里的"光标移动"用，不走平滑）。
pub fn move_now(dx: f64, dy: f64) {
    flush_motion();
    apply_move(dx, dy);
}

/// 点击、滚轮之前先把还没走完的位移一次走完，免得点在半路上。
pub fn flush_motion() {
    let mut playout = PLAYOUT.lock().unwrap();
    let (dx, dy) = playout.flush();
    apply_move(dx, dy);
}

fn now_ms() -> f64 {
    EPOCH.elapsed().as_secs_f64() * 1000.0
}

fn play_loop() {
    let mut playout = PLAYOUT.lock().unwrap();
    loop {
        while playout.is_empty() {
            playout = WAKE.wait(playout).unwrap();
        }
        let (dx, dy) = playout.take(now_ms());
        // 拿着锁走：flush_motion 插不进来，指针不会先走后面的、再走前面的
        apply_move(dx, dy);
        drop(playout);
        std::thread::sleep(PLAY_TICK);
        playout = PLAYOUT.lock().unwrap();
    }
}

fn apply_move(dx: f64, dy: f64) {
    let (ix, iy) = {
        let mut fraction = FRACTION.lock().unwrap();
        fraction.0 += dx;
        fraction.1 += dy;
        let (ix, iy) = (fraction.0.trunc(), fraction.1.trunc());
        fraction.0 -= ix;
        fraction.1 -= iy;
        (ix as i32, iy as i32)
    };
    if ix == 0 && iy == 0 {
        return;
    }
    crate::trace::event("cursor", &[ix as f64, iy as f64]);
    if crate::trace::dry_run() {
        return;
    }
    let mut point = POINT::default();
    unsafe {
        if GetCursorPos(&mut point).is_ok() {
            let _ = SetCursorPos(point.x + ix, point.y + iy);
        }
    }
}

pub fn button(button: Button, action: &str) {
    flush_motion();
    let (down, up, data) = button.flags();
    let mut held = HELD.lock().unwrap();
    match action {
        "down" => {
            if !held.contains(&button) {
                held.push(button);
            }
            send(&[mouse_input(down, data)]);
        }
        "up" => {
            held.retain(|b| *b != button);
            send(&[mouse_input(up, data)]);
        }
        _ => send(&[mouse_input(down, data), mouse_input(up, data)]),
    }
}

/// 有没有按着的鼠标键或键盘键（看门狗用）
pub fn any_held() -> bool {
    !HELD.lock().unwrap().is_empty() || !KEYS.lock().unwrap().order.is_empty()
}

/// 松开所有按着的鼠标键和键盘键（断线、息屏、看门狗、退出）
pub fn release_all() {
    let held = std::mem::take(&mut *HELD.lock().unwrap());
    for button in held {
        let (_, up, data) = button.flags();
        send(&[mouse_input(up, data)]);
    }
    release_keys_all();
}

pub fn scroll(dx: i32, dy: i32) {
    flush_motion();
    let mut inputs = Vec::new();
    if dy != 0 {
        inputs.push(mouse_input(MOUSEEVENTF_WHEEL, dy));
    }
    if dx != 0 {
        inputs.push(mouse_input(MOUSEEVENTF_HWHEEL, dx));
    }
    send(&inputs);
}

/// 按键名换成虚拟键码（名字表在 phonepad_config::key_code，和 axonkey 一样）；有一个不认识就返回 None
pub fn key_codes(names: &[String]) -> Option<Vec<u16>> {
    let mut vks: Vec<u16> = Vec::new();
    for name in names {
        let vk = phonepad_config::key_code(name)?;
        if !vks.contains(&vk) {
            vks.push(vk);
        }
    }
    (!vks.is_empty()).then_some(vks)
}

/// 按一次键（可带修饰键）。按键名不认识时返回 false。
pub fn key(name: &str, mods: &[String]) -> bool {
    let mut names = mods.to_vec();
    names.push(name.to_string());
    match key_codes(&names) {
        Some(vks) => {
            tap(&vks);
            true
        }
        None => false,
    }
}

/// 依次按下、倒序松开。已经被「直通」按着的键不再按、也不松开（不然会把用户正按着的修饰键抬起来）
pub fn tap(vks: &[u16]) {
    let held: Vec<u16> = KEYS.lock().unwrap().order.clone();
    let pressed: Vec<u16> = vks.iter().copied().filter(|vk| !held.contains(vk)).collect();
    let mut seq: Vec<INPUT> = pressed.iter().map(|&vk| key_input(vk, false)).collect();
    seq.extend(pressed.iter().rev().map(|&vk| key_input(vk, true)));
    send(&seq);
}

/// 「直通」按着的键：引用计数、按下的先后、下一次补发按下的时间
#[derive(Default)]
struct HeldKeys {
    counts: HashMap<u16, usize>,
    order: Vec<u16>,
    repeat_due: HashMap<u16, Instant>,
    /// 全部松开（断开、暂停、改配置……）一次加一：之前按下的那些，手机再说松开时不能把之后新按下的键也松掉
    generation: u64,
}

static KEYS: LazyLock<Mutex<HeldKeys>> = LazyLock::new(Default::default);
static KEYS_WAKE: Condvar = Condvar::new();
static REPEATER: OnceLock<()> = OnceLock::new();
const KEY_REPEAT_DELAY: Duration = Duration::from_millis(400);
const KEY_REPEAT_INTERVAL: Duration = Duration::from_millis(80);

/// 按住时要不要像实体键盘一样补发按下：修饰键、媒体键不补发，音量加减补发（和 axonkey 一样）
fn auto_repeats(vk: u16) -> bool {
    matches!(vk, 0xAE | 0xAF) || !(phonepad_config::actions::is_modifier(vk) || (0xAD..=0xB3).contains(&vk))
}

/// 按下并一直按着（直通）。同一个键被两个手机按键按着时只按一次，见 release_keys
pub fn hold_keys(vks: &[u16]) -> u64 {
    REPEATER.get_or_init(|| {
        std::thread::Builder::new().name("key-repeat".into()).spawn(repeat_loop).ok();
    });
    let mut keys = KEYS.lock().unwrap();
    let mut downs = Vec::new();
    for &vk in vks {
        let count = keys.counts.entry(vk).or_default();
        *count += 1;
        if *count == 1 {
            keys.order.push(vk);
            downs.push(key_input(vk, false));
            if auto_repeats(vk) {
                keys.repeat_due.insert(vk, Instant::now() + KEY_REPEAT_DELAY);
            }
        }
    }
    send(&downs);
    KEYS_WAKE.notify_all();
    keys.generation
}

/// 松开 hold_keys 按着的键（倒序）；最后一个引用松开时才真的抬起。
/// generation 是按下时 hold_keys 返回的：中间已经全部松开过的话，这些键早就抬起了，什么都不做
pub fn release_keys(vks: &[u16], generation: u64) {
    let mut keys = KEYS.lock().unwrap();
    if keys.generation != generation {
        return;
    }
    let mut ups = Vec::new();
    for &vk in vks.iter().rev() {
        let Some(count) = keys.counts.get_mut(&vk) else { continue };
        *count -= 1;
        if *count == 0 {
            keys.counts.remove(&vk);
            keys.order.retain(|k| *k != vk);
            keys.repeat_due.remove(&vk);
            ups.push(key_input(vk, true));
        }
    }
    send(&ups);
}

/// 松开所有直通按着的键（倒序）
pub fn release_keys_all() {
    let mut keys = KEYS.lock().unwrap();
    let ups: Vec<INPUT> = keys.order.iter().rev().map(|&vk| key_input(vk, true)).collect();
    *keys = HeldKeys { generation: keys.generation + 1, ..Default::default() };
    send(&ups);
}

/// 按着的非修饰键：400 ms 后每 80 ms 补发一次按下（Windows 不会替注入的按键自动连发）
fn repeat_loop() {
    let mut keys = KEYS.lock().unwrap();
    loop {
        let now = Instant::now();
        let mut downs = Vec::new();
        let mut next: Option<Instant> = None;
        for (vk, due) in keys.repeat_due.iter_mut() {
            if *due <= now {
                downs.push(key_input(*vk, false));
                *due = now + KEY_REPEAT_INTERVAL;
            }
            next = Some(next.map_or(*due, |n: Instant| n.min(*due)));
        }
        if !downs.is_empty() {
            send(&downs);
        }
        keys = match next {
            Some(due) => KEYS_WAKE.wait_timeout(keys, due.saturating_duration_since(Instant::now())).unwrap().0,
            None => KEYS_WAKE.wait(keys).unwrap(),
        };
    }
}

/// 逐字发 Unicode 字符：不经过电脑输入法，也不动剪贴板。换行发 Shift+Enter。
pub fn type_text(text: &str) {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    for (i, line) in normalized.split('\n').enumerate() {
        if i > 0 {
            key("enter", &["shift".to_string()]);
        }
        let seq: Vec<INPUT> = line.encode_utf16().flat_map(|u| [char_input(u, false), char_input(u, true)]).collect();
        for chunk in seq.chunks(200) {
            send(chunk);
        }
    }
}

/// 放进剪贴板再按 Ctrl+V；过一秒把原来的剪贴板内容（图片、文件等全部格式）原样放回去，见 clipboard.rs。
/// 剪贴板借不了（里面有存不下来的东西、被别的程序占着）时改成逐字输入
pub fn paste_text(text: &str) {
    let pasted = crate::clipboard::paste_with(text, crate::clipboard::RESTORE_AFTER, || {
        key("v", &["ctrl".to_string()]);
    });
    if !pasted {
        type_text(text);
    }
}
