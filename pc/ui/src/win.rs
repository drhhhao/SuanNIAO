//! 主界面用到的 Windows 小功能：只开一个窗口（新版换掉开着的旧版）、顶栏当标题栏（安装界面也用）、系统要重画时整个窗口重画、启动和换掉后台、选文件、开机自启、打开文件夹、蓝牙自检。
//!
//! 文件对话框放在单独的线程里开（file_dialog）：在界面的回调里直接开的话，对话框自己的消息循环和界面的事件循环、
//! 定时器互相唤醒，对话框开着时会占满一个 CPU 核。

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::mem::{size_of, ManuallyDrop};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Duration;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::ComponentHandle;
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Devices::Bluetooth::{
    BluetoothFindDeviceClose, BluetoothFindFirstDevice, BluetoothFindFirstRadio, BluetoothFindNextDevice, BluetoothFindRadioClose,
    BluetoothGetRadioInfo, BLUETOOTH_DEVICE_INFO, BLUETOOTH_DEVICE_SEARCH_PARAMS, BLUETOOTH_FIND_RADIO_PARAMS, BLUETOOTH_RADIO_INFO,
};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, GlobalFree, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HANDLE, HGLOBAL, HWND, LPARAM,
    LRESULT, POINT, RECT, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, GetUpdateRect, InvalidateRect, MonitorFromWindow, ScreenToClient, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetActiveWindow, TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentThreadId, OpenMutexW, OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, SYNCHRONIZATION_SYNCHRONIZE,
};
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_NODEREFERENCELINKS, OFN_OVERWRITEPROMPT,
    OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows::Win32::UI::Shell::{
    DefSubclassProc, RemoveWindowSubclass, SHAppBarMessage, SetWindowSubclass, ShellExecuteW, ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP,
    ABM_GETAUTOHIDEBAREX, ABM_GETSTATE, ABS_AUTOHIDE, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, EnumWindows, FindWindowExW, FindWindowW, GetClientRect, GetPropW, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId,
    IsIconic,
    IsZoomed, PostMessageW, RegisterWindowMessageW, RemovePropW, SetForegroundWindow, SetPropW, SetWindowPos, SetWindowsHookExW, ShowWindow,
    UnhookWindowsHookEx,
    CBT_CREATEWNDW, CREATESTRUCTW, GWL_EXSTYLE, GWL_STYLE, HCBT_CREATEWND, HHOOK, HTCAPTION, HTCLIENT, HTMAXBUTTON, HTTOP, HTTOPLEFT,
    HTTOPRIGHT, MINMAXINFO, SC_CLOSE, SC_MAXIMIZE, SC_MINIMIZE, SC_RESTORE, SIZE_MINIMIZED, SM_CXPADDEDBORDER, SM_CYSIZEFRAME,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SW_RESTORE, SW_SHOWNORMAL, WH_CBT,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_GETMINMAXINFO, WM_MOUSEMOVE, WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCDESTROY,
    WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_PAINT, WM_SIZE, WM_SYSCOMMAND,
    WS_THICKFRAME,
};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
/// 主界面占着的名字（互斥量），也是挂在主窗口上的属性名，属性的值是主界面的版本号（见 version_mark）。
/// 测试用的副本把这个名字整个换掉，就既不占正在用的名字，也认不到正在用的窗口
const UI_LOCK: PCWSTR = w!("Local\\SuanNIAO.UI");
/// 后台占着的名字（互斥量）和它托盘窗口的类名
const CORE_LOCK: PCWSTR = w!("Local\\SuanNIAO.Core");
const CORE_TRAY: PCWSTR = w!("SuanNIAOTrayWindow");
/// 开机自启在 Run 里的名字
const RUN_VALUE: PCWSTR = w!("SuanNIAO");
/// 改名前（0.7.4 以前内部叫 PhonePad）的这些名字：新版启动时认出旧的主界面、后台和开机自启，换成新的
const LEGACY_UI_LOCK: PCWSTR = w!("Local\\PhonePad.UI");
const LEGACY_CORE_LOCK: PCWSTR = w!("Local\\PhonePad.Core");
const LEGACY_CORE_TRAY: PCWSTR = w!("PhonePadTrayWindow");
const LEGACY_RUN_VALUE: PCWSTR = w!("PhonePad");
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;

pub fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default()
}

fn core_exe() -> PathBuf {
    exe_dir().join("SuanNIAO-core.exe")
}

/// 已经开着一个主界面时，把它提到前台并返回 true（这个进程直接退出）；show_update 时还让它打开更新窗口（`--show-update`）。
/// 开着的比这个版本旧（刚更新完，旧窗口还开着）就让它关掉，这个接着启动，返回 false
pub fn activate_existing_window(show_update: bool) -> bool {
    unsafe {
        // 句柄故意不关：进程活着时一直占着这个名字（换掉旧窗口以后也是）
        let _ = CreateMutexW(None, true, UI_LOCK);
        if GetLastError() != ERROR_ALREADY_EXISTS {
            close_legacy_ui();
            return false;
        }
        let running = running_ui(UI_LOCK, None);
        if let Some((hwnd, version)) = running {
            if version < version_mark() && close_and_wait(hwnd) {
                return false;
            }
        }
        if let Some(hwnd) = running.map(|(hwnd, _)| hwnd).or_else(|| FindWindowW(PCWSTR::null(), w!("蒜鸟")).ok()) {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
            if show_update {
                let _ = PostMessageW(Some(hwnd), show_update_message(), WPARAM(0), LPARAM(0));
            }
        }
        true
    }
}

/// 让开着的主界面打开更新窗口（另一个 `SuanNIAO.exe --show-update` 发过来）
fn show_update_message() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe { RegisterWindowMessageW(w!("SuanNIAO.ShowUpdate")) })
}

/// 挂在主窗口上的版本号：0.7.4 → 7004，比大小就是比版本
fn version_mark() -> usize {
    let (major, minor, patch) = crate::version_tuple(env!("CARGO_PKG_VERSION"));
    (major as usize * 1000 + minor as usize) * 1000 + patch as usize
}

/// 改名前的主界面（PhonePad.exe）还开着：一定比这个版本旧，让它关掉
fn close_legacy_ui() {
    if mutex_exists(LEGACY_UI_LOCK) {
        if let Some((hwnd, _)) = running_ui(LEGACY_UI_LOCK, None) {
            close_and_wait(hwnd);
        }
    }
}

/// 开着的主界面窗口和它挂的版本号（属性名是 lock）。dir 不是 None 时只要那个文件夹里的程序开的。
/// 0.7.3 以前的没挂版本号（当作 0）：认标题「蒜鸟」和 winit 的窗口类，还要是 dir（没给就是这个程序所在的文件夹）里的程序开的（不碰别处的副本）
fn running_ui(lock: PCWSTR, dir: Option<&Path>) -> Option<(HWND, usize)> {
    struct Search<'a> {
        lock: PCWSTR,
        dir: Option<&'a Path>,
        found: Option<(HWND, usize)>,
    }
    unsafe extern "system" fn marked(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        unsafe {
            let search = &mut *(lparam.0 as *mut Search<'_>);
            let version = GetPropW(hwnd, search.lock).0 as usize;
            if version == 0 || search.dir.is_some_and(|dir| !window_in(hwnd, dir)) {
                return true.into();
            }
            search.found = Some((hwnd, version));
            false.into()
        }
    }
    let mut search = Search { lock, dir, found: None };
    unsafe {
        let _ = EnumWindows(Some(marked), LPARAM(&mut search as *mut Search<'_> as isize));
    }
    if search.found.is_some() {
        return search.found;
    }
    let here = dir.map(Path::to_path_buf).unwrap_or_else(exe_dir);
    let mut after = None;
    while let Ok(hwnd) = unsafe { FindWindowExW(None, after, w!("Window Class"), w!("蒜鸟")) } {
        if window_in(hwnd, &here) {
            return Some((hwnd, 0));
        }
        after = Some(hwnd);
    }
    None
}

/// 窗口是不是 dir 里的程序开的
fn window_in(hwnd: HWND, dir: &Path) -> bool {
    window_exe_dir(hwnd).is_some_and(|exe_dir| same_dir(&exe_dir, dir))
}

/// 窗口是哪个文件夹里的程序开的
fn window_exe_dir(hwnd: HWND) -> Option<PathBuf> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut path = [0u16; 1024];
        let mut len = path.len() as u32;
        let result = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(path.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        result.ok()?;
        PathBuf::from(OsString::from_wide(&path[..len as usize])).parent().map(Path::to_path_buf)
    }
}

/// 大小写、短文件名不一样也算同一个文件夹
pub fn same_dir(a: &Path, b: &Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

/// 让开着的旧主界面关掉（和点它右上角的 × 一样），最多等 5 秒。关掉了返回 true
fn close_and_wait(hwnd: HWND) -> bool {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else { return false };
        let closed = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok() && WaitForSingleObject(process, 5000) == WAIT_OBJECT_0;
        let _ = CloseHandle(process);
        closed
    }
}

/// 自绘标题栏要知道的窗口状态（hook_main_window 的 frame_changed 收到的）
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    /// 系统的标题栏去掉了、顶栏当标题栏用。没接管成功时是 false：系统的标题栏还在，界面上就不画窗口按钮
    pub custom: bool,
    pub maximized: bool,
    /// 窗口在前台（不在前台时窗口按钮淡一点，和系统的标题栏一样）
    pub active: bool,
    /// 鼠标在最大化按钮上、按着最大化按钮。这个按钮上的鼠标消息不经过 Slint（Windows 11 要在这里弹贴靠布局），由这里告诉界面
    pub max_hover: bool,
    pub max_pressed: bool,
}

/// 主窗口要接管的事（见 hook_main_window）。安装界面也这样接管，只是 main 是 false
pub struct MainWindow {
    /// 是蒜鸟的主界面：挂上版本号（以后的新版认得出它），别的 `SuanNIAO.exe --show-update` 能叫它打开更新窗口
    pub main: bool,
    /// 别的 `SuanNIAO.exe --show-update` 叫这个窗口打开更新窗口（排到事件循环里调）
    pub show_update: Box<dyn Fn()>,
    /// 系统要重画一块地方时调：让整个窗口都算变了
    pub redraw_all: Box<dyn Fn()>,
    /// 鼠标在窗口的哪里（逻辑像素，从窗口左上角算）：0 窗口内容，1 标题栏（拖动、双击最大化、右键系统菜单），2 最大化按钮
    pub title_hit: Box<dyn Fn(f32, f32) -> i32>,
    /// 窗口状态变了（不在 Windows 的消息里直接调，排到事件循环里）
    pub frame_changed: Box<dyn Fn(Frame)>,
    /// 1 个逻辑像素是几个物理像素
    pub scale: Box<dyn Fn() -> f32>,
}

struct Hook {
    window: MainWindow,
    frame: Cell<Frame>,
    /// 已经排了一次 frame_changed，还没执行
    queued: Cell<bool>,
}

/// 还没接上主窗口
struct Pending {
    /// 等窗口建出来的钩子（没装上时是 None，只能等事件循环跑起来再接）
    cbt: Option<HHOOK>,
    hook: Rc<Hook>,
}

thread_local! {
    static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) };
    /// 接管了的主窗口，窗口按钮（window_command）用
    static MAIN_HWND: Cell<isize> = const { Cell::new(0) };
}

/// 接管主窗口：
/// - 不用系统的标题栏（和程序别的地方不是一个样子），顶栏当标题栏用。系统先按普通窗口算边框，再把上面的标题栏收回来给窗口内容，
///   所以左右下的边框还能拖着改大小，阴影、圆角、贴靠都还在；最上面一条（系统边框那么高）也还能改大小。
///   鼠标在哪里由界面说了算（title_hit）：顶栏空白处是标题栏，可以拖、双击最大化、右键出系统菜单；
///   最大化按钮告诉系统是最大化按钮，Windows 11 悬停时会弹贴靠布局。
/// - Slint 只把它自己知道变了的地方交给系统。Windows 要窗口重画时（窗口藏起来又显示、移出屏幕的部分又移回来、
///   显示器睡醒、远程桌面换了显示设置……），没变的地方它一个像素也不交，那里就是透明的，露出窗口后面的东西。
///   所以系统要重画一块地方时先调 redraw_all（让整个窗口都算变了），再照常交给 Slint 画。
/// - 主界面的窗口上挂这个版本的版本号：以后的新版打开时认出开着的是旧版，把它换掉（见 activate_existing_window）。
/// - 大小固定的窗口（安装界面）最上面那条不能改大小。
///
/// 在窗口建出来的那一刻接上（WH_CBT 钩子，只看界面线程）：Slint 建好窗口马上画第一帧、显示出来，晚了会先闪一下系统的标题栏。
/// 万一没等到（窗口类名变了之类），事件循环跑起来以后再接，接上后让系统重算边框
pub fn hook_main_window<T: ComponentHandle + 'static>(ui: slint::Weak<T>, window: MainWindow) {
    let frame = Frame { custom: true, active: true, ..Default::default() };
    (window.frame_changed)(frame);
    let hook = Rc::new(Hook { window, frame: Cell::new(frame), queued: Cell::new(false) });
    let cbt = unsafe { SetWindowsHookExW(WH_CBT, Some(cbt_proc), None, GetCurrentThreadId()) }.ok();
    PENDING.set(Some(Pending { cbt, hook }));
    slint::Timer::single_shot(Duration::ZERO, move || attach_late(ui, 100));
    // 窗口是事件循环跑起来以后才建的，建好了再看要不要缩
    slint::Timer::single_shot(Duration::ZERO, || fit_fixed_window(40));
}

/// 大小固定的窗口（安装界面）：系统按带标题栏算的窗口大小，标题栏收回来给了窗口内容，内容就多出标题栏那么高。缩回去
fn fit_fixed_window(tries: u32) {
    let hwnd = MAIN_HWND.get();
    if hwnd == 0 {
        if tries > 0 {
            slint::Timer::single_shot(Duration::from_millis(25), move || fit_fixed_window(tries - 1));
        }
        return;
    }
    let hwnd = HWND(hwnd as *mut _);
    if resizable(hwnd) {
        return;
    }
    unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_ok() {
            let flags = SWP_NOMOVE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE;
            let _ = SetWindowPos(hwnd, None, 0, 0, rect.right - rect.left, rect.bottom - rect.top - caption_height(hwnd), flags);
            // 已经画过一帧的话，缩完整个重画（系统要重画时 main_window_proc 让 Slint 整个窗口都画）
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

/// 等主窗口建出来：建出来的那一刻（还没收到任何消息）接上
unsafe extern "system" fn cbt_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code == HCBT_CREATEWND as i32 && is_main_window(&*(*(lparam.0 as *const CBT_CREATEWNDW)).lpcs) {
            if let Some(pending) = PENDING.take() {
                if let Some(cbt) = pending.cbt {
                    let _ = UnhookWindowsHookEx(cbt);
                }
                if !attach(HWND(wparam.0 as *mut _), pending.hook.clone()) {
                    // 等事件循环跑起来再试（attach_late）
                    PENDING.set(Some(Pending { cbt: None, hook: pending.hook }));
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

/// 要建的是不是主窗口：没有父窗口，窗口类是 winit 的（类名也可能是个整数编号，那样不是指针，不能读）
fn is_main_window(create: &CREATESTRUCTW) -> bool {
    create.hwndParent.0.is_null()
        && create.lpszClass.0 as usize > 0xFFFF
        && unsafe { create.lpszClass.to_string() }.is_ok_and(|class| class == "Window Class")
}

/// 建窗口时没接上的话，等窗口建好再接，接上后让系统按没有标题栏重算边框；一直接不上就留着系统的标题栏
fn attach_late<T: ComponentHandle + 'static>(ui: slint::Weak<T>, tries: u32) {
    if PENDING.with_borrow(Option::is_none) {
        return;
    }
    let hwnd = ui.upgrade().and_then(|strong| match strong.window().window_handle().window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => Some(HWND(h.hwnd.get() as *mut _)),
        _ => None,
    });
    if hwnd.is_none() && tries > 0 {
        slint::Timer::single_shot(Duration::from_millis(50), move || attach_late(ui, tries - 1));
        return;
    }
    let Some(pending) = PENDING.take() else { return };
    if let Some(cbt) = pending.cbt {
        unsafe {
            let _ = UnhookWindowsHookEx(cbt);
        }
    }
    match hwnd {
        Some(hwnd) if attach(hwnd, pending.hook.clone()) => unsafe {
            let flags = SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE;
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, flags);
        },
        _ => set_frame(&pending.hook, Frame { custom: false, ..pending.hook.frame.get() }),
    }
}

/// hook 在窗口销毁时（WM_NCDESTROY）由 main_window_proc 放掉，挂的版本号也是那时摘掉
fn attach(hwnd: HWND, hook: Rc<Hook>) -> bool {
    let main = hook.window.main;
    let data = Rc::into_raw(hook);
    unsafe {
        if SetWindowSubclass(hwnd, Some(main_window_proc), 1, data as usize).as_bool() {
            MAIN_HWND.set(hwnd.0 as isize);
            if main {
                // 以后的新版打开时靠它认出这个窗口是旧版（见 activate_existing_window）
                let _ = SetPropW(hwnd, UI_LOCK, Some(HANDLE(version_mark() as *mut _)));
            }
            true
        } else {
            drop(Rc::from_raw(data));
            false
        }
    }
}

/// 记下窗口状态，变了就排到事件循环里告诉界面（在 Windows 的消息里直接改界面的话，有可能正赶上 Slint 自己在改窗口）
fn set_frame(hook: &Rc<Hook>, frame: Frame) {
    if hook.frame.replace(frame) == frame || hook.queued.replace(true) {
        return;
    }
    let hook = hook.clone();
    slint::Timer::single_shot(Duration::ZERO, move || {
        hook.queued.set(false);
        (hook.window.frame_changed)(hook.frame.get());
    });
}

fn set_max_button(hook: &Rc<Hook>, hover: bool, pressed: bool) {
    set_frame(hook, Frame { max_hover: hover, max_pressed: pressed, ..hook.frame.get() });
}

unsafe extern "system" fn main_window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, id: usize, data: usize) -> LRESULT {
    let hook = ManuallyDrop::new(unsafe { Rc::from_raw(data as *const Hook) });
    unsafe {
        match msg {
            // 系统先按普通窗口算（左右下的边框不变），再把上面的标题栏收回来给窗口内容。
            // wparam 是 0 时 lparam 指的就是那个矩形，不是 0 时是 NCCALCSIZE_PARAMS，它的第一个字段也是这个矩形
            WM_NCCALCSIZE => {
                let rect = lparam.0 as *mut RECT;
                let top = (*rect).top;
                let result = DefSubclassProc(hwnd, msg, wparam, lparam);
                if result.0 == 0 {
                    (*rect).top = top;
                    if IsZoomed(hwnd).as_bool() {
                        // 最大化时窗口四边都伸出屏幕一圈边框那么宽，上面这圈不能算进窗口内容
                        (*rect).top += frame_thickness(hwnd);
                        leave_room_for_hidden_taskbar(hwnd, &mut *rect);
                    }
                }
                return result;
            }
            WM_NCHITTEST => {
                // 左右下的边框系统认得
                let hit = DefSubclassProc(hwnd, msg, wparam, lparam);
                if hit.0 != HTCLIENT as isize {
                    return hit;
                }
                let mut point = POINT { x: lparam.0 as i16 as i32, y: (lparam.0 >> 16) as i16 as i32 };
                let _ = ScreenToClient(hwnd, &mut point);
                // 最上面一条是改大小的边框（最大化时、大小固定的窗口没有），两头斜着改
                let border = frame_thickness(hwnd);
                if point.y < border && !IsZoomed(hwnd).as_bool() && resizable(hwnd) {
                    let mut client = RECT::default();
                    let _ = GetClientRect(hwnd, &mut client);
                    let code = if point.x < border * 2 {
                        HTTOPLEFT
                    } else if point.x >= client.right - border * 2 {
                        HTTOPRIGHT
                    } else {
                        HTTOP
                    };
                    return LRESULT(code as isize);
                }
                let scale = (hook.window.scale)();
                return match (hook.window.title_hit)(point.x as f32 / scale, point.y as f32 / scale) {
                    1 => LRESULT(HTCAPTION as isize),
                    2 => LRESULT(HTMAXBUTTON as isize),
                    _ => hit,
                };
            }
            WM_NCMOUSEMOVE => {
                let over = wparam.0 == HTMAXBUTTON as usize;
                if over {
                    // 鼠标离开这一格时要收到 WM_NCMOUSELEAVE
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE | TME_NONCLIENT,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut track);
                }
                set_max_button(&hook, over, over && hook.frame.get().max_pressed);
            }
            WM_NCMOUSELEAVE | WM_MOUSEMOVE => set_max_button(&hook, false, false),
            // 最大化按钮自己处理，不交给系统：系统会按它自己的（看不见的）最大化按钮的位置来
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if wparam.0 == HTMAXBUTTON as usize => {
                set_max_button(&hook, true, true);
                return LRESULT(0);
            }
            WM_NCLBUTTONUP if wparam.0 == HTMAXBUTTON as usize => {
                if hook.frame.get().max_pressed {
                    set_max_button(&hook, true, false);
                    window_command(WindowCommand::ToggleMaximize);
                }
                return LRESULT(0);
            }
            WM_SIZE => {
                let mut frame = Frame { max_hover: false, max_pressed: false, ..hook.frame.get() };
                // 最小化时不改：从最大化最小化的，还原回来还是最大化
                if wparam.0 != SIZE_MINIMIZED as usize {
                    frame.maximized = IsZoomed(hwnd).as_bool();
                }
                set_frame(&hook, frame);
            }
            WM_NCACTIVATE => set_frame(&hook, Frame { active: wparam.0 != 0, ..hook.frame.get() }),
            WM_GETMINMAXINFO => {
                // winit 按带系统标题栏的窗口算最小尺寸（大小固定的窗口还有最大尺寸），标题栏收回来以后多出标题栏那么高
                let result = DefSubclassProc(hwnd, msg, wparam, lparam);
                let info = lparam.0 as *mut MINMAXINFO;
                let caption = caption_height(hwnd);
                if (*info).ptMinTrackSize.y > caption * 2 {
                    (*info).ptMinTrackSize.y -= caption;
                }
                if !resizable(hwnd) && (*info).ptMaxTrackSize.y > caption * 2 {
                    (*info).ptMaxTrackSize.y -= caption;
                }
                return result;
            }
            // 有要重画的区域才是系统要的；Slint 自己要重画时（RDW_INTERNALPAINT）没有区域，照常只画变了的地方
            WM_PAINT if GetUpdateRect(hwnd, None, false).as_bool() => (hook.window.redraw_all)(),
            WM_NCDESTROY => {
                let _ = RemoveWindowSubclass(hwnd, Some(main_window_proc), id);
                let _ = RemovePropW(hwnd, UI_LOCK);
                MAIN_HWND.set(0);
                drop(ManuallyDrop::into_inner(hook));
            }
            message if message == show_update_message() && hook.window.main => {
                let hook = Rc::clone(&hook);
                slint::Timer::single_shot(Duration::ZERO, move || (hook.window.show_update)());
                return LRESULT(0);
            }
            _ => {}
        }
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }
}

/// 窗口能拖着改大小（大小固定的窗口 winit 会去掉 WS_THICKFRAME）
fn resizable(hwnd: HWND) -> bool {
    unsafe { GetWindowLongW(hwnd, GWL_STYLE) as u32 & WS_THICKFRAME.0 != 0 }
}

/// 窗口四边看不见的那圈边框（拖着改大小的地方）有多宽
fn frame_thickness(hwnd: HWND) -> i32 {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) + GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi)
    }
}

/// 系统的标题栏连上面的边框有多高（按窗口现在的样式和 DPI 算）
fn caption_height(hwnd: HWND) -> i32 {
    unsafe {
        let style = WINDOW_STYLE(GetWindowLongW(hwnd, GWL_STYLE) as u32);
        let ex_style = WINDOW_EX_STYLE(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32);
        let mut rect = RECT::default();
        let _ = AdjustWindowRectExForDpi(&mut rect, style, false, ex_style, GetDpiForWindow(hwnd));
        -rect.top
    }
}

/// 任务栏设成自动隐藏时，最大化的窗口在任务栏那一边让出 2 像素：
/// 盖满整个屏幕的话系统当它是全屏程序，鼠标移到屏幕边上任务栏也不出来
fn leave_room_for_hidden_taskbar(hwnd: HWND, rect: &mut RECT) {
    unsafe {
        let mut state = APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, ..Default::default() };
        if SHAppBarMessage(ABM_GETSTATE, &mut state) as u32 & ABS_AUTOHIDE == 0 {
            return;
        }
        let mut monitor = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut monitor).as_bool() {
            return;
        }
        let bar_on = |edge: u32| {
            let mut bar = APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, uEdge: edge, rc: monitor.rcMonitor, ..Default::default() };
            SHAppBarMessage(ABM_GETAUTOHIDEBAREX, &mut bar) != 0
        };
        if bar_on(ABE_TOP) {
            rect.top += 2;
        }
        if bar_on(ABE_BOTTOM) {
            rect.bottom -= 2;
        }
        if bar_on(ABE_LEFT) {
            rect.left += 2;
        }
        if bar_on(ABE_RIGHT) {
            rect.right -= 2;
        }
    }
}

/// 顶栏右边的窗口按钮
#[derive(Clone, Copy, Debug)]
pub enum WindowCommand {
    Minimize,
    /// 最大化，已经最大化了就还原
    ToggleMaximize,
    Close,
}

/// 和点系统标题栏上的按钮一样（发 WM_SYSCOMMAND）：有动画，还原时回到原来的位置和大小
pub fn window_command(command: WindowCommand) {
    let hwnd = MAIN_HWND.get();
    if hwnd == 0 {
        return;
    }
    let hwnd = HWND(hwnd as *mut _);
    unsafe {
        let code = match command {
            WindowCommand::Minimize => SC_MINIMIZE,
            WindowCommand::ToggleMaximize if IsZoomed(hwnd).as_bool() => SC_RESTORE,
            WindowCommand::ToggleMaximize => SC_MAXIMIZE,
            WindowCommand::Close => SC_CLOSE,
        };
        let _ = PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(code as usize), LPARAM(0));
    }
}

/// 后台在跑没有（后台启动时会占一个固定名字的互斥量）。
pub fn core_running() -> bool {
    mutex_exists(CORE_LOCK)
}

fn mutex_exists(name: PCWSTR) -> bool {
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, name) {
            Ok(handle) => {
                let _ = CloseHandle(handle);
                true
            }
            Err(_) => false,
        }
    }
}

pub fn start_core() -> bool {
    start_core_in(&exe_dir())
}

/// 打开 dir 里的后台（安装程序装完用）
pub fn start_core_in(dir: &Path) -> bool {
    std::process::Command::new(dir.join("SuanNIAO-core.exe")).creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS).spawn().is_ok()
}

pub fn ensure_core_running() {
    if !core_running() {
        start_core();
    }
}

/// 让正在跑的后台退出：给它的托盘窗口发「退出」菜单命令（和托盘右键点「退出」一样，会先松开按着的键），
/// 最多等 3 秒。退出了返回 true
pub fn stop_core() -> bool {
    stop_core_named(CORE_LOCK, CORE_TRAY)
}

/// `SuanNIAO.exe --quit`（0.7.6 以前的安装程序覆盖文件、卸载之前用）：这个文件夹里的程序开着的主界面和后台都关掉
pub fn quit_running() {
    quit_running_in(&exe_dir());
}

/// 关掉 dir 里的程序开着的主界面和后台（安装覆盖文件、卸载之前）。别的文件夹里的副本开着的不碰。
/// 返回后台刚才在不在跑（装完再打开它）
pub fn quit_running_in(dir: &Path) -> bool {
    if let Some((hwnd, _)) = running_ui(UI_LOCK, Some(dir)) {
        close_and_wait(hwnd);
    }
    let core = unsafe { FindWindowW(CORE_TRAY, PCWSTR::null()) }.is_ok_and(|hwnd| window_in(hwnd, dir));
    if core {
        stop_core();
    }
    core
}

/// 改名前的后台（phonepad-core.exe）还在跑：一定比这个版本旧，让它退出。
/// 要在搬数据文件夹之前：它开着日志文件，文件夹改不了名
pub fn stop_legacy_core() {
    if mutex_exists(LEGACY_CORE_LOCK) {
        stop_core_named(LEGACY_CORE_LOCK, LEGACY_CORE_TRAY);
    }
}

fn stop_core_named(lock: PCWSTR, tray: PCWSTR) -> bool {
    // 托盘菜单里「退出」的命令号，各版本的后台都一样
    const MENU_EXIT: usize = 2;
    unsafe {
        if let Ok(hwnd) = FindWindowW(tray, PCWSTR::null()) {
            let _ = PostMessageW(Some(hwnd), WM_COMMAND, WPARAM(MENU_EXIT), LPARAM(0));
        }
    }
    for _ in 0..30 {
        if !mutex_exists(lock) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

pub fn open_path(path: &Path) {
    unsafe {
        let _ = ShellExecuteW(None, w!("open"), &HSTRING::from(path.as_os_str()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// 文件对话框：选程序、选遥控器映射文件、选备份，另存为（建议的文件名）
pub enum FileDialog {
    App,
    RemoteMapping,
    Backup,
    SaveJson(String),
    /// 选文件夹（从这个文件夹开始）
    Folder(PathBuf),
}

/// 在单独的线程里开文件对话框，选完（或取消）以后在界面线程里调 done。
/// 对话框的主人是现在的主窗口：开着的时候主窗口点不了
pub fn file_dialog(kind: FileDialog, done: impl FnOnce(Option<String>) + Send + 'static) {
    let owner = unsafe { GetActiveWindow() }.0 as isize;
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let owner = HWND(owner as *mut _);
        let result = match kind {
            // 快捷方式返回 .lnk 本身的路径
            FileDialog::App => open_dialog(owner, "程序和快捷方式 (*.exe;*.lnk;*.com)\0*.exe;*.lnk;*.com\0所有文件 (*.*)\0*.*\0\0", true),
            FileDialog::RemoteMapping => open_dialog(owner, "遥控器映射文件 (*.json)\0*.json\0所有文件 (*.*)\0*.*\0\0", false),
            FileDialog::Backup => open_dialog(owner, "蒜鸟备份 (*.json)\0*.json\0所有文件 (*.*)\0*.*\0\0", false),
            FileDialog::SaveJson(name) => save_json(owner, &name),
            FileDialog::Folder(start) => pick_folder(owner, &start),
        };
        let _ = slint::invoke_from_event_loop(move || done(result));
    });
}

/// 选文件夹的对话框（系统的新式对话框）
fn pick_folder(owner: HWND, start: &Path) -> Option<String> {
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS,
        SIGDN_FILESYSPATH,
    };
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        let _ = dialog.SetTitle(w!("手机发来的文件存到哪个文件夹"));
        // 文件夹还没建（第一次用）时从它的上一层开始
        let existing = start.ancestors().find(|p| p.is_dir());
        if let Some(dir) = existing {
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(dir.as_os_str()), None) {
                let _ = dialog.SetFolder(&item);
            }
        }
        dialog.Show(Some(owner)).ok()?;
        let path = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let text = path.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(path.0 as *const _));
        text
    }
}

/// 系统里「下载」文件夹的位置（可能被挪到了别的盘，比如 E:\Downloads）
pub fn downloads_dir() -> PathBuf {
    use windows::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, KNOWN_FOLDER_FLAG};
    unsafe {
        if let Ok(path) = SHGetKnownFolderPath(&FOLDERID_Downloads, KNOWN_FOLDER_FLAG(0), None) {
            let text = path.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(path.0 as *const _));
            if !text.is_empty() {
                return PathBuf::from(text);
            }
        }
    }
    PathBuf::from(std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into())).join("Downloads")
}

/// 在资源管理器里打开文件所在的文件夹，并选中这个文件
pub fn reveal(path: &str) {
    let _ = std::process::Command::new("explorer").arg(format!("/select,{path}")).spawn();
}

/// 另存为对话框（JSON 文件）：default_name 是建议的文件名，已经有同名文件时会问要不要覆盖
fn save_json(owner: HWND, default_name: &str) -> Option<String> {
    let mut buffer = [0u16; 1024];
    for (i, c) in default_name.encode_utf16().take(buffer.len() - 1).enumerate() {
        buffer[i] = c;
    }
    let filter: Vec<u16> = "JSON 文件 (*.json)\0*.json\0\0".encode_utf16().collect();
    let extension: Vec<u16> = "json\0".encode_utf16().collect();
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buffer.as_mut_ptr()),
        nMaxFile: buffer.len() as u32,
        lpstrDefExt: PCWSTR(extension.as_ptr()),
        Flags: OFN_OVERWRITEPROMPT | OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST,
        ..Default::default()
    };
    if unsafe { GetSaveFileNameW(&mut dialog) }.as_bool() {
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..len]))
    } else {
        None
    }
}

/// 本地时间比 UTC 早多少秒
pub fn local_offset_secs() -> i64 {
    use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
    let (local, utc) = unsafe { (GetLocalTime(), GetSystemTime()) };
    let minutes = |t: &windows::Win32::Foundation::SYSTEMTIME| (t.wDay as i64 * 24 + t.wHour as i64) * 60 + t.wMinute as i64;
    // 本地和 UTC 差几分钟（日期跨月时按差值最小的那个算）
    let mut offset = minutes(&local) - minutes(&utc);
    for wrap in [-28 * 1440, -29 * 1440, -30 * 1440, -31 * 1440, 28 * 1440, 29 * 1440, 30 * 1440, 31 * 1440] {
        if (offset + wrap).abs() < offset.abs() {
            offset += wrap;
        }
    }
    offset * 60
}

/// UNIX 秒 → 本地时间「2026-10-29 07:45」（按现在的时区换算）
pub fn local_time_text(secs: u64) -> String {
    let shifted = (secs as i64 + local_offset_secs()).max(0) as u64;
    let iso = suanniao_config::transfer::iso_utc(shifted);
    iso.replace('T', " ").trim_end_matches('Z').chars().take(16).collect()
}

/// 本地时间，文件名用：20261029-071500
pub fn file_stamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// 剪贴板打开不了（别的程序正占着）时稍等重试
fn open_clipboard() -> bool {
    for _ in 0..25 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// 把文字放进剪贴板（用户点了「复制动作」：和 Ctrl+C 一样替换剪贴板里原来的内容）。
/// 先把内存准备好再清空剪贴板，免得清空了却放不进去
pub fn set_clipboard_text(text: &str) -> bool {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let Ok(memory) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) else { return false };
        let target = GlobalLock(memory) as *mut u16;
        if target.is_null() {
            let _ = GlobalFree(Some(memory));
            return false;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), target, wide.len());
        let _ = GlobalUnlock(memory);
        if !open_clipboard() {
            let _ = GlobalFree(Some(memory));
            return false;
        }
        let _ = EmptyClipboard();
        // 放进剪贴板以后内存归系统管；没放进去才自己释放
        let ok = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(memory.0))).is_ok();
        let _ = CloseClipboard();
        if !ok {
            let _ = GlobalFree(Some(memory));
        }
        ok
    }
}

/// 剪贴板里的文字（没有文字返回 None）。只读，不改剪贴板
pub fn get_clipboard_text() -> Option<String> {
    if !open_clipboard() {
        return None;
    }
    let text = unsafe {
        GetClipboardData(CF_UNICODETEXT.0 as u32).ok().and_then(|handle| {
            let memory = HGLOBAL(handle.0);
            // 最多读到内存块结尾，没有结尾的 0 也不会越界
            let units = GlobalSize(memory) / 2;
            let source = GlobalLock(memory) as *const u16;
            if source.is_null() {
                return None;
            }
            let all = std::slice::from_raw_parts(source, units);
            let len = all.iter().position(|&u| u == 0).unwrap_or(units);
            let text = String::from_utf16_lossy(&all[..len]);
            let _ = GlobalUnlock(memory);
            Some(text)
        })
    };
    unsafe {
        let _ = CloseClipboard();
    }
    text
}

/// 打开文件对话框。keep_links：选到快捷方式时返回 .lnk 本身，不换成它指向的文件
fn open_dialog(owner: HWND, filter: &str, keep_links: bool) -> Option<String> {
    let mut buffer = [0u16; 1024];
    let filter: Vec<u16> = filter.encode_utf16().collect();
    let mut flags = OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR;
    if keep_links {
        flags |= OFN_NODEREFERENCELINKS;
    }
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buffer.as_mut_ptr()),
        nMaxFile: buffer.len() as u32,
        Flags: flags,
        ..Default::default()
    };
    if unsafe { GetOpenFileNameW(&mut dialog) }.as_bool() {
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..len]))
    } else {
        None
    }
}

pub fn autostart_enabled() -> bool {
    run_value_exists(RUN_VALUE)
}

fn run_value_exists(name: PCWSTR) -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, name, RRF_RT_REG_SZ, None, None, None) == ERROR_SUCCESS }
}

/// 开机自启：在当前用户的 Run 里写上后台程序的路径。
pub fn set_autostart(on: bool) -> bool {
    unsafe {
        if on {
            let value = format!("\"{}\"", core_exe().display());
            let wide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                RUN_VALUE,
                REG_SZ.0,
                Some(wide.as_ptr().cast()),
                (wide.len() * 2) as u32,
            ) == ERROR_SUCCESS
        } else {
            let result = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
            result == ERROR_SUCCESS || result == ERROR_FILE_NOT_FOUND
        }
    }
}

/// 改名前开了开机自启（Run 里的 PhonePad，指着 phonepad-core.exe）：换成新的，指着这个文件夹里的后台
pub fn move_legacy_autostart() {
    if run_value_exists(LEGACY_RUN_VALUE) && set_autostart(true) {
        unsafe {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, LEGACY_RUN_VALUE);
        }
    }
}

/// 电脑蓝牙的情况（首页连接自检用）
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bluetooth {
    /// 找到了开着的蓝牙
    pub radio: bool,
    /// 这台电脑在蓝牙里的名字（手机的「选择电脑」里列的就是它）；没找到蓝牙时是空的
    pub name: String,
    /// 配对过的手机的名字
    pub phones: Vec<String>,
    /// 配对过的设备一共几个
    pub paired: usize,
}

/// 看一下电脑蓝牙开没开、配对过哪些手机（只读系统记着的，不搜索附近的设备，很快）
pub fn bluetooth() -> Bluetooth {
    let mut state = Bluetooth::default();
    unsafe {
        let params = BLUETOOTH_FIND_RADIO_PARAMS { dwSize: std::mem::size_of::<BLUETOOTH_FIND_RADIO_PARAMS>() as u32 };
        let mut radio = HANDLE::default();
        if let Ok(find) = BluetoothFindFirstRadio(&params, &mut radio) {
            state.radio = true;
            let mut info = BLUETOOTH_RADIO_INFO { dwSize: std::mem::size_of::<BLUETOOTH_RADIO_INFO>() as u32, ..Default::default() };
            if BluetoothGetRadioInfo(radio, &mut info) == ERROR_SUCCESS.0 {
                let len = info.szName.iter().position(|&c| c == 0).unwrap_or(info.szName.len());
                state.name = String::from_utf16_lossy(&info.szName[..len]).trim().to_string();
            }
            let _ = BluetoothFindRadioClose(find);
            let _ = CloseHandle(radio);
        }
        let search = BLUETOOTH_DEVICE_SEARCH_PARAMS {
            dwSize: std::mem::size_of::<BLUETOOTH_DEVICE_SEARCH_PARAMS>() as u32,
            fReturnAuthenticated: true.into(),
            fReturnRemembered: true.into(),
            fReturnUnknown: false.into(),
            fReturnConnected: true.into(),
            fIssueInquiry: false.into(),
            cTimeoutMultiplier: 0,
            hRadio: HANDLE::default(),
        };
        let fresh = || BLUETOOTH_DEVICE_INFO { dwSize: std::mem::size_of::<BLUETOOTH_DEVICE_INFO>() as u32, ..Default::default() };
        let mut info = fresh();
        if let Ok(find) = BluetoothFindFirstDevice(&search, &mut info) {
            loop {
                if info.fAuthenticated.as_bool() || info.fRemembered.as_bool() {
                    state.paired += 1;
                    // 设备类别的主类：2 是手机
                    if (info.ulClassofDevice >> 8) & 0x1F == 2 {
                        let len = info.szName.iter().position(|&c| c == 0).unwrap_or(info.szName.len());
                        state.phones.push(String::from_utf16_lossy(&info.szName[..len]));
                    }
                }
                info = fresh();
                if BluetoothFindNextDevice(find, &mut info).is_err() {
                    break;
                }
            }
            let _ = BluetoothFindDeviceClose(find);
        }
    }
    state
}
