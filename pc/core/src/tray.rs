//! 托盘图标：左键单击打开主界面，右键菜单有「打开蒜鸟」「暂停按键 / 恢复按键」和「退出」。提示文字显示手机连接状态。

use std::mem::size_of;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, NOTIFY_ICON_MESSAGE,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::{actions, input, link, log, overlay, state};

const WM_TRAY: u32 = WM_APP + 1;
const MENU_OPEN: usize = 1;
const MENU_EXIT: usize = 2;
const MENU_PAUSE: usize = 3;

static WINDOW: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static TOOLTIP: Mutex<String> = Mutex::new(String::new());

/// 建托盘图标并跑消息循环，直到用户选「退出」。
pub fn run() {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("SuanNIAOTrayWindow");
        let wc = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: instance.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("SuanNIAOTray"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        ) else {
            log!("❌ 建托盘窗口失败");
            return;
        };
        WINDOW.store(hwnd.0 as isize, Ordering::Relaxed);
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);
        if TOOLTIP.lock().unwrap().is_empty() {
            *TOOLTIP.lock().unwrap() = "蒜鸟 · 未连接".into();
        }
        notify(NIM_ADD);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        notify(NIM_DELETE);
    }
}

/// 更新托盘提示文字（任何线程都可以调）。
pub fn set_tooltip(text: &str) {
    *TOOLTIP.lock().unwrap() = text.to_string();
    notify(NIM_MODIFY);
}

fn notify(op: NOTIFY_ICON_MESSAGE) {
    let hwnd = WINDOW.load(Ordering::Relaxed);
    if hwnd == 0 {
        return;
    }
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(hwnd as *mut _),
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon(),
        ..Default::default()
    };
    let tip: Vec<u16> = TOOLTIP.lock().unwrap().encode_utf16().take(data.szTip.len() - 1).collect();
    data.szTip[..tip.len()].copy_from_slice(&tip);
    unsafe {
        let _ = Shell_NotifyIconW(op, &data);
    }
}

/// 托盘要小图标（150% 缩放下是 24px）：直接从 exe 的图标里挑这个尺寸，免得系统拿大图缩、缩得发糊。
/// 载一次就留着用到进程退出
fn icon() -> HICON {
    static ICON: OnceLock<usize> = OnceLock::new();
    let handle = *ICON.get_or_init(|| unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let (w, h) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        match LoadImageW(Some(instance.into()), PCWSTR(1 as _), IMAGE_ICON, w, h, LR_DEFAULTCOLOR) {
            Ok(icon) => icon.0 as usize,
            Err(_) => LoadIconW(None, IDI_APPLICATION).map(|icon| icon.0 as usize).unwrap_or_default(),
        }
    });
    HICON(handle as *mut _)
}

/// 给托盘窗口发个消息（别的线程要在托盘线程上做事时用，比如弹切换方案的通知）
pub fn post(msg: u32) {
    let hwnd = WINDOW.load(Ordering::Relaxed);
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(0), LPARAM(0));
        }
    }
}

/// 打开主界面（和后台放在同一个文件夹的 SuanNIAO.exe）。
pub fn open_ui() {
    let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) else { return };
    let exe = dir.join("SuanNIAO.exe");
    if let Err(e) = std::process::Command::new(&exe).spawn() {
        log!("⚠ 打不开主界面 {}：{e}", exe.display());
    }
}

/// 暂停按键 / 恢复按键：写进配置（主界面会发现、手机上会显示），暂停时取消排队的动作、松开按着的键。
/// 托盘菜单和手机上的「暂停按键」都走这里；from 是谁让改的（写日志用）
pub fn set_enabled(enabled: bool, from: &str) {
    match state::set_mapping_enabled(enabled) {
        Ok(()) => {
            if !enabled {
                actions::cancel();
                input::release_keys_all();
            }
            link::push_config();
            log!("{} {from}：{}", if enabled { "▶" } else { "⏸" }, if enabled { "恢复按键" } else { "暂停按键" });
        }
        Err(e) => log!("⚠ 改总开关失败：{e}"),
    }
}

fn toggle_pause() {
    set_enabled(!state::config().settings.enabled, "托盘");
}

fn show_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("打开蒜鸟"));
        let paused = !state::config().settings.enabled;
        let _ = AppendMenuW(menu, MF_STRING, MENU_PAUSE, if paused { w!("恢复按键") } else { w!("暂停按键") });
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("退出"));
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd); // 不先激活，点菜单外面时菜单不会自动收起
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, point.x, point.y, None, hwnd, None);
        let _ = DestroyMenu(menu);
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_TRAY {
        match (lparam.0 & 0xFFFF) as u32 {
            WM_LBUTTONUP => open_ui(),
            WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
            _ => {}
        }
        return LRESULT(0);
    }
    if msg == overlay::WM_SHOW_OVERLAY {
        overlay::on_show();
        return LRESULT(0);
    }
    if msg == WM_COMMAND {
        match wparam.0 & 0xFFFF {
            MENU_OPEN => open_ui(),
            MENU_PAUSE => toggle_pause(),
            MENU_EXIT => PostQuitMessage(0),
            _ => {}
        }
        return LRESULT(0);
    }
    let taskbar_created = TASKBAR_CREATED.load(Ordering::Relaxed);
    if taskbar_created != 0 && msg == taskbar_created {
        notify(NIM_ADD); // 资源管理器重启后托盘图标会丢，重新加上
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
