//! 打开应用、打开网站。
//!
//! - 网站：只认 http、https（不写协议补 https://），交给系统默认浏览器。
//! - 应用：路径要写全；工作目录设成文件所在的文件夹（和 axonkey 一样），可以带启动参数。
//! - activate（用户自改版 axonkey 的「已在运行就切到它的窗口」）：.lnk 先解析出目标程序、参数和工作目录；
//!   按程序名找进程的主窗口（顶层、可见、没有 owner、不是工具窗口），最小化的先还原、藏在托盘里的先显示，
//!   再借前台线程的输入队列调到前台；没在运行就启动，每 150 ms 看一次、最多 10 秒，窗口出来后调到前台。
//!   这些都在单独的线程里做，不耽误后面的动作。
//! - 后台进程打开的程序、网页默认抢不到前台（Windows 的前台锁，只在任务栏闪一下）：打开以后在单独的线程里等它的窗口
//!   （新启动的程序：新进程的主窗口；网页：默认浏览器新开的窗口，或者标题变了的窗口，也就是切到了新标签），
//!   出来了就用上面的办法调到前台。等不到就算了，不乱抢别的窗口。
//!
//! ShellExecute 要在 COM 单线程套间里调：执行线程已经初始化了，activate 的线程自己初始化。

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use phonepad_config::actions::{app_path_problem, normalize_url};
use windows::core::{w, Interface, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::core::PWSTR;
use windows::Win32::UI::Shell::{AssocQueryStringW, IShellLinkW, ShellExecuteW, ShellLink, ASSOCF_NONE, ASSOCSTR_EXECUTABLE};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, SetForegroundWindow, ShowWindow, GWL_EXSTYLE, GW_OWNER, SW_RESTORE, SW_SHOW, SW_SHOWNORMAL, WS_EX_TOOLWINDOW,
};

use crate::log;

const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(10);
const ACTIVATE_POLL: Duration = Duration::from_millis(150);
/// 浏览器打开网址后多久内切到新标签（窗口标题变了）才调到前台
const BROWSER_TIMEOUT: Duration = Duration::from_secs(5);

pub fn open_website(url: &str) -> Result<(), String> {
    let url = normalize_url(url).ok_or_else(|| format!("网址不对：{url}（只能是 http 或 https）"))?;
    // 先记下默认浏览器现在有哪些窗口、标题是什么，打开以后看哪个是新的
    let browser = default_browser();
    let before = browser.as_deref().map(browser_windows).unwrap_or_default();
    shell_open(&url, "", None)?;
    log!("🌐 打开网站 {url}");
    if let Some(exe) = browser {
        let _ = std::thread::Builder::new().name("browser-front".into()).spawn(move || browser_to_front(&exe, &before));
    }
    Ok(())
}

pub fn open_app(path: &str, args: &str, activate: bool) -> Result<(), String> {
    let path = path.trim().trim_matches('"').to_string();
    if let Some(problem) = app_path_problem(&path) {
        return Err(format!("{problem}：{path}"));
    }
    let args = args.to_string();
    std::thread::Builder::new()
        .name("open-app".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            launch(&path, &args, activate);
            unsafe { CoUninitialize() };
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn shell_open(target: &str, args: &str, dir: Option<&str>) -> Result<(), String> {
    let args = HSTRING::from(args);
    let dir = dir.map(HSTRING::from);
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(target),
            if args.is_empty() { PCWSTR::null() } else { PCWSTR(args.as_ptr()) },
            dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            SW_SHOWNORMAL,
        )
    };
    if (result.0 as isize) <= 32 {
        return Err(format!("Windows 打不开（错误 {}）：{target}", result.0 as isize));
    }
    Ok(())
}

/// 打开应用。activate：已在运行就把它的窗口调到前台，不再开一个。
/// 启动以后等新进程的窗口出来（最多 10 秒）再调到前台
fn launch(path: &str, args: &str, activate: bool) {
    let (target, args, dir) = if activate && path.to_ascii_lowercase().ends_with(".lnk") {
        // 要按程序名找窗口：先看快捷方式指向哪个程序
        match resolve_shortcut(path) {
            Some((target, link_args, dir)) => (target, if args.is_empty() { link_args } else { args.to_string() }, dir),
            None => (path.to_string(), args.to_string(), String::new()),
        }
    } else {
        let dir = Path::new(path).parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
        (path.to_string(), args.to_string(), dir)
    };
    if activate && activate_window(&target, true, None) {
        log!("🔁 {target} 已经在运行，切到它的窗口");
        return;
    }
    let before = exe_name(&target).map(|name| process_ids(&name)).unwrap_or_default();
    if let Err(e) = shell_open(&target, &args, (!dir.is_empty()).then_some(dir.as_str())) {
        log!("⚠ 打开应用失败：{e}");
        return;
    }
    log!("🚀 打开 {target}{}", if args.is_empty() { String::new() } else { format!("（参数 {args}）") });
    // 只有 .exe 能按程序名找到窗口（快捷方式没解析、文档交给别的程序打开了）
    if !target.to_ascii_lowercase().ends_with(".exe") {
        return;
    }
    let deadline = Instant::now() + ACTIVATE_TIMEOUT;
    while Instant::now() < deadline {
        std::thread::sleep(ACTIVATE_POLL);
        // 只认这次新启动的进程，同一个程序早就开着的窗口不算
        let pids = exe_name(&target).map(|name| process_ids(&name)).unwrap_or_default();
        let fresh: HashSet<u32> = pids.difference(&before).copied().collect();
        if !fresh.is_empty() && activate_window(&target, false, Some(&fresh)) {
            return;
        }
    }
    log!("⚠ {target} 10 秒内没出现新窗口，没调到前台");
}

fn exe_name(path: &str) -> Option<String> {
    Path::new(path).file_name().map(|n| n.to_string_lossy().to_lowercase())
}

/// 系统默认浏览器（https 的默认程序）的路径
fn default_browser() -> Option<String> {
    let mut buffer = [0u16; 1040];
    let mut len = buffer.len() as u32;
    unsafe { AssocQueryStringW(ASSOCF_NONE, ASSOCSTR_EXECUTABLE, w!("https"), w!("open"), Some(PWSTR(buffer.as_mut_ptr())), &mut len) }.ok().ok()?;
    let path = from_wide(&buffer);
    path.to_ascii_lowercase().ends_with(".exe").then_some(path)
}

/// 浏览器的主窗口和它们的标题（标题是当前标签页的标题）
fn browser_windows(exe: &str) -> Vec<(isize, String)> {
    let Some(name) = exe_name(exe) else { return Vec::new() };
    let pids = process_ids(&name);
    if pids.is_empty() {
        return Vec::new();
    }
    let mut search = Search { pids: &pids, visible: None, hidden: None, all: Some(Vec::new()) };
    unsafe {
        let _ = EnumWindows(Some(main_window), LPARAM(&mut search as *mut Search as isize));
    }
    search
        .all
        .unwrap_or_default()
        .into_iter()
        .map(|hwnd| {
            let mut title = [0u16; 512];
            let n = unsafe { GetWindowTextW(hwnd, &mut title) }.max(0) as usize;
            (hwnd.0 as isize, String::from_utf16_lossy(&title[..n]))
        })
        .collect()
}

/// 浏览器收到网址后会新开窗口或者切到新标签（标题变了）：等到了就调到前台
fn browser_to_front(exe: &str, before: &[(isize, String)]) {
    let deadline = Instant::now() + BROWSER_TIMEOUT;
    while Instant::now() < deadline {
        std::thread::sleep(ACTIVATE_POLL);
        let changed = browser_windows(exe)
            .into_iter()
            .find(|(hwnd, title)| !title.is_empty() && !before.iter().any(|(h, t)| h == hwnd && t == title));
        if let Some((hwnd, _)) = changed {
            let hwnd = HWND(hwnd as *mut _);
            unsafe {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
            }
            force_foreground(hwnd);
            return;
        }
    }
}

/// 解析快捷方式：（目标，参数，工作目录）
fn resolve_shortcut(path: &str) -> Option<(String, String, String)> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        file.Load(&HSTRING::from(path), STGM_READ).ok()?;
        let mut target = [0u16; 1040];
        let mut args = [0u16; 1040];
        let mut dir = [0u16; 1040];
        link.GetPath(&mut target, std::ptr::null_mut(), 0).ok()?;
        let _ = link.GetArguments(&mut args);
        let _ = link.GetWorkingDirectory(&mut dir);
        let target = from_wide(&target);
        (!target.is_empty()).then(|| (target, from_wide(&args), from_wide(&dir)))
    }
}

struct Search<'a> {
    pids: &'a HashSet<u32>,
    visible: Option<HWND>,
    hidden: Option<HWND>,
    /// 要的话收集所有看得见的主窗口
    all: Option<Vec<HWND>>,
}

/// 找到程序的主窗口并调到前台。allow_hidden：藏在托盘里的窗口也算（刚启动时不算，让程序自己先把窗口亮出来）；
/// only：只看这些进程（刚启动的）
fn activate_window(exe: &str, allow_hidden: bool, only: Option<&HashSet<u32>>) -> bool {
    let Some(name) = exe_name(exe) else { return false };
    let pids = match only {
        Some(only) => only.clone(),
        None => process_ids(&name),
    };
    if pids.is_empty() {
        return false;
    }
    let mut search = Search { pids: &pids, visible: None, hidden: None, all: None };
    unsafe {
        let _ = EnumWindows(Some(main_window), LPARAM(&mut search as *mut Search as isize));
    }
    let hidden = if allow_hidden { search.hidden } else { None };
    let Some(hwnd) = search.visible.or(hidden) else { return false };
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        } else if search.visible.is_none() {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
    }
    force_foreground(hwnd);
    true
}

unsafe extern "system" fn main_window(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    unsafe {
        let search = &mut *(lparam.0 as *mut Search);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if !search.pids.contains(&pid) {
            return true.into();
        }
        // 主窗口：顶层、没有 owner、不是工具窗口
        if GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.is_invalid()) {
            return true.into();
        }
        if GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return true.into();
        }
        if IsWindowVisible(hwnd).as_bool() {
            search.visible.get_or_insert(hwnd);
            if let Some(all) = search.all.as_mut() {
                all.push(hwnd);
            }
        } else {
            search.hidden.get_or_insert(hwnd);
        }
        true.into()
    }
}

/// 后台线程直接 SetForegroundWindow 会被 Windows 拒绝：先接到前台线程和目标窗口线程的输入队列上再调
fn force_foreground(hwnd: HWND) {
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground.is_invalid() {
            let _ = SetForegroundWindow(hwnd);
            return;
        }
        let me = GetCurrentThreadId();
        let fg_thread = GetWindowThreadProcessId(foreground, None);
        let target_thread = GetWindowThreadProcessId(hwnd, None);
        let attached_fg = fg_thread != me && AttachThreadInput(fg_thread, me, true).as_bool();
        let attached_target = target_thread != me && AttachThreadInput(target_thread, me, true).as_bool();
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        if attached_fg {
            let _ = AttachThreadInput(fg_thread, me, false);
        }
        if attached_target {
            let _ = AttachThreadInput(target_thread, me, false);
        }
    }
}

fn process_ids(exe_name: &str) -> HashSet<u32> {
    let mut pids = HashSet::new();
    let own = std::process::id();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return pids };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                if entry.th32ProcessID != own && from_wide(&entry.szExeFile).to_lowercase() == exe_name {
                    pids.insert(entry.th32ProcessID);
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    pids
}

fn from_wide(value: &[u16]) -> String {
    let len = value.iter().position(|&c| c == 0).unwrap_or(value.len());
    String::from_utf16_lossy(&value[..len])
}
