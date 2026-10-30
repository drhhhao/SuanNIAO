//! 安装、更新、卸载蒜鸟（`SuanNIAO.exe --setup` / `--uninstall`）。
//!
//! 安装包（pc/setup）把 SuanNIAO.exe、SuanNIAO-core.exe 解到临时文件夹，运行里面的 `SuanNIAO.exe --setup`：
//! 安装界面是主界面程序的另一个窗口（app.slint 的 SetupWindow），和蒜鸟一个样子。
//! - `--setup [--update] [--dir 文件夹] [--silent] [--no-desktop]`：装到 dir（默认是登记过的位置，没装过就是
//!   %LOCALAPPDATA%\Programs\SuanNIAO），只给当前用户装、不要管理员权限。先关掉那个文件夹里开着的蒜鸟，换掉两个 exe，
//!   开始菜单（和桌面）放「蒜鸟」，在系统的「应用」里登记（卸载入口）。
//!   安装界面上能换文件夹（「更改」）；装过的换了文件夹就是搬家：装到新位置，再删掉原来那份。
//!   --update 是主界面下载好新版以后打开的：不问、直接装到主界面所在的文件夹，装好打开新版。
//! - `--uninstall [--silent] [--remove-data]`：系统「应用」里点卸载时运行装好的 SuanNIAO.exe。
//!
//! 登记用的 AppId 和以前的 Inno Setup 安装包一样（0.7.6 装的也认得、卸载得掉），定了不能改。
//! 测试：环境变量 SUANNIAO_SETUP_SANDBOX=某个文件夹 时，开始菜单、桌面、注册表都换到测试用的地方，
//! 不打开装好的程序，窗口也放在屏幕外面。

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, SharedString, Timer};
use suanniao_config as config;
use suanniao_update as update;
use windows::core::{w, Interface, GUID, HSTRING};
use windows::Win32::Foundation::{ERROR_SUCCESS, POINT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{GetStartupInfoW, STARTF_USEPOSITION, STARTUPINFOW};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Programs, FOLDERID_UserProgramFiles, IShellLinkW, SHGetKnownFolderPath, ShellLink, KF_FLAG_DONT_VERIFY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_XVIRTUALSCREEN};

use crate::{win, SetupWindow};

/// 在系统里登记用的 AppId：和以前的 Inno Setup 安装包一样，定了不能改（改了就认不出装过的蒜鸟、卸载不掉旧的）
const APP_ID: &str = "{DE1A7C69-32AD-4AD1-BF71-2B9B63532E2F}";
/// 装的文件
const FILES: [&str; 2] = ["SuanNIAO.exe", "SuanNIAO-core.exe"];
/// 开始菜单、桌面上的快捷方式
const LINK_NAME: &str = "蒜鸟.lnk";
/// 开机自启（Run 里的名字，同 win.rs）
const RUN_VALUE: &str = "SuanNIAO";
const SANDBOX_ENV: &str = "SUANNIAO_SETUP_SANDBOX";
const PICK_ENV: &str = "SUANNIAO_SETUP_PICK";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 安装窗口的大小（和 app.slint 里 SetupWindow 的一样），放到屏幕中间用
const WINDOW_SIZE: (f32, f32) = (720.0, 460.0);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Install,
    Update,
    Uninstall,
}

struct Options {
    mode: Mode,
    dir: Option<PathBuf>,
    silent: bool,
    desktop: bool,
    remove_data: bool,
}

impl Options {
    fn parse(args: &[String]) -> Self {
        let mut options = Self { mode: Mode::Install, dir: None, silent: false, desktop: true, remove_data: false };
        let mut args = args.iter().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--uninstall" => options.mode = Mode::Uninstall,
                "--update" if options.mode != Mode::Uninstall => options.mode = Mode::Update,
                "--dir" => options.dir = args.next().filter(|dir| !dir.trim().is_empty()).map(PathBuf::from),
                "--silent" => options.silent = true,
                "--no-desktop" => options.desktop = false,
                "--remove-data" => options.remove_data = true,
                _ => {}
            }
        }
        options
    }
}

/// 开始菜单、桌面、注册表的位置（测试时换到测试用的地方）
#[derive(Clone, Debug)]
struct Places {
    programs: PathBuf,
    desktop: PathBuf,
    /// HKEY_CURRENT_USER 下面
    uninstall_key: String,
    run_key: String,
    default_dir: PathBuf,
    /// 测试用的文件夹（SUANNIAO_SETUP_SANDBOX）
    sandbox: Option<PathBuf>,
}

impl Places {
    fn get() -> Self {
        if let Some(root) = std::env::var_os(SANDBOX_ENV).filter(|root| !root.is_empty()).map(PathBuf::from) {
            return Self {
                programs: root.join("Programs"),
                desktop: root.join("Desktop"),
                uninstall_key: format!("Software\\SuanNIAO-SetupTest\\Uninstall\\{APP_ID}_is1"),
                run_key: "Software\\SuanNIAO-SetupTest\\Run".into(),
                default_dir: root.join("SuanNIAO"),
                sandbox: Some(root),
            };
        }
        let env_dir = |name: &str, tail: &str| std::env::var_os(name).map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(tail);
        Self {
            programs: known_folder(&FOLDERID_Programs)
                .unwrap_or_else(|| env_dir("APPDATA", "Microsoft\\Windows\\Start Menu\\Programs")),
            desktop: known_folder(&FOLDERID_Desktop).unwrap_or_else(|| env_dir("USERPROFILE", "Desktop")),
            uninstall_key: format!("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{APP_ID}_is1"),
            run_key: "Software\\Microsoft\\Windows\\CurrentVersion\\Run".into(),
            default_dir: known_folder(&FOLDERID_UserProgramFiles).unwrap_or_else(|| env_dir("LOCALAPPDATA", "Programs")).join("SuanNIAO"),
            sandbox: None,
        }
    }
}

/// 一次安装、更新或卸载要做的事（在别的线程里做）
#[derive(Clone, Debug)]
struct Job {
    mode: Mode,
    /// 要装的文件在哪（安装包解出来的临时文件夹）
    from: PathBuf,
    dir: PathBuf,
    /// 搬家：原来装在这里（登记过的那份），装好新的以后删掉
    old_dir: Option<PathBuf>,
    desktop: bool,
    remove_data: bool,
    places: Places,
}

/// 运行安装、更新或卸载，返回进程的退出码：0 好了，1 出错，2 没开始就关掉了
pub fn run(args: &[String]) -> i32 {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let options = Options::parse(args);
    let places = Places::get();
    if options.mode == Mode::Uninstall {
        // 别占着要删的文件夹
        let _ = std::env::set_current_dir(std::env::temp_dir());
    }
    let dir = match (&options.dir, options.mode) {
        (Some(dir), _) => std::path::absolute(dir).unwrap_or_else(|_| dir.clone()),
        (None, Mode::Uninstall) => win::exe_dir(),
        (None, _) => registered_dir(&places).unwrap_or_else(|| places.default_dir.clone()),
    };
    let installed = installed_version(&dir, &places);
    // 桌面上放不放：没装过默认放；装过的看现在桌面上有没有
    let desktop = options.desktop && (installed.is_none() || points_into(&places.desktop.join(LINK_NAME), &dir));
    let job = Job { mode: options.mode, from: win::exe_dir(), dir, old_dir: None, desktop, remove_data: options.remove_data, places };
    let code = if options.silent { silent(&job) } else { window(job.clone(), installed) };
    if job.mode == Mode::Uninstall && code == 0 {
        delete_self_later(&job.dir);
    }
    code
}

/// 不开窗口，直接做（`--silent`）。更新的话装好打开新版
fn silent(job: &Job) -> i32 {
    match job.run(&|_, _| {}) {
        Ok(()) => {
            if job.mode == Mode::Update {
                launch(job);
            }
            0
        }
        Err(_) => 1,
    }
}

/// 打开装好的蒜鸟（测试时不打开）
fn launch(job: &Job) {
    if job.places.sandbox.is_none() {
        let _ = std::process::Command::new(job.dir.join("SuanNIAO.exe")).current_dir(&job.dir).spawn();
    }
}

// ───────────────────────── 窗口 ─────────────────────────

struct Shared {
    ui: slint::Weak<SetupWindow>,
    job: RefCell<Job>,
    /// 一开始要装到的文件夹（装过的话就是装着的地方）
    original: PathBuf,
    /// 原来装着蒜鸟，而且是登记过的那份：换了文件夹就搬家
    movable: bool,
    /// 关不了窗口（正在装，或者马上要装）
    busy: Cell<bool>,
    /// 正在装（别的线程在做）
    running: Cell<bool>,
    code: Cell<i32>,
}

thread_local! {
    static SETUP: RefCell<Option<Rc<Shared>>> = const { RefCell::new(None) };
}

fn with_setup(f: impl FnOnce(&Rc<Shared>)) {
    let shared = SETUP.with(|s| s.borrow().clone());
    if let Some(shared) = shared {
        f(&shared);
    }
}

fn window(job: Job, installed: Option<String>) -> i32 {
    let Ok(ui) = SetupWindow::new() else { return silent(&job) };
    let compare = match installed.as_deref() {
        Some(version) if version != "?" && update::newer(version, VERSION) => -1,
        Some(version) if version != "?" && !update::newer(VERSION, version) => 0,
        _ => 1,
    };
    ui.set_mode(match job.mode {
        Mode::Install => 0,
        Mode::Update => 1,
        Mode::Uninstall => 2,
    });
    ui.set_version(VERSION.into());
    ui.set_installed(installed.clone().unwrap_or_default().into());
    ui.set_compare(compare);
    ui.set_folder(job.dir.display().to_string().into());
    ui.set_data_folder(config::data_dir().display().to_string().into());
    ui.set_desktop(job.desktop);
    ui.set_remove_data(job.remove_data);
    let update_mode = job.mode == Mode::Update;
    let original = job.dir.clone();
    let movable = installed.is_some() && registered_dir(&job.places).is_some_and(|r| same_place(&r, &original));
    let shared = Rc::new(Shared { ui: ui.as_weak(), job: RefCell::new(job), original, movable, busy: Cell::new(false), running: Cell::new(false), code: Cell::new(2) });
    SETUP.with(|s| *s.borrow_mut() = Some(shared.clone()));

    // 和主界面一样：系统的标题栏换成自己画的，系统要重画时整个窗口重画
    let (repaint, hit, frame, scale) = (ui.as_weak(), ui.as_weak(), ui.as_weak(), ui.as_weak());
    win::hook_main_window(
        ui.as_weak(),
        win::MainWindow {
            main: false,
            show_update: Box::new(|| {}),
            redraw_all: Box::new(move || {
                if let Some(ui) = repaint.upgrade() {
                    ui.set_repaint_all(!ui.get_repaint_all());
                }
            }),
            title_hit: Box::new(move |x, y| hit.upgrade().map_or(0, |ui| ui.invoke_title_hit(x, y))),
            frame_changed: Box::new(move |f| {
                if let Some(ui) = frame.upgrade() {
                    ui.set_custom_frame(f.custom);
                    ui.set_window_active(f.active);
                }
            }),
            scale: Box::new(move || scale.upgrade().map_or(1.0, |ui| ui.window().scale_factor())),
        },
    );
    ui.on_minimize_window(|| win::window_command(win::WindowCommand::Minimize));
    ui.on_close_window(|| win::window_command(win::WindowCommand::Close));
    ui.on_start_setup(|| with_setup(start));
    ui.on_change_folder(|| with_setup(change_folder));
    ui.on_finish(|| with_setup(|s| s.close()));
    ui.on_launch(|| {
        with_setup(|s| {
            launch(&s.job.borrow());
            s.close();
        })
    });
    {
        let shared = shared.clone();
        ui.window().on_close_requested(move || {
            if shared.busy.get() {
                slint::CloseRequestResponse::KeepWindowShown
            } else {
                slint::CloseRequestResponse::HideWindow
            }
        });
    }
    let sandbox = shared.job.borrow().places.sandbox.is_some();
    place_window(&ui, sandbox);
    // 测试：SUANNIAO_SETUP_PICK=文件夹 当成在「更改」里选了它，=dialog 打开选文件夹的对话框（窗口在屏幕外面，点不了）
    if let Some(pick) = std::env::var(PICK_ENV).ok().filter(|_| sandbox) {
        Timer::single_shot(Duration::from_millis(600), move || {
            with_setup(|s| if pick == "dialog" { change_folder(s) } else { s.folder_picked(Path::new(&pick)) })
        });
    }
    if update_mode {
        // 主界面下载好新版以后打开的：不用问，直接装
        ui.set_page(1);
        ui.set_busy(true);
        shared.busy.set(true);
        Timer::single_shot(Duration::from_millis(400), || with_setup(start));
    }
    let _ = ui.run();
    SETUP.with(|s| s.borrow_mut().take());
    shared.code.get()
}

impl Shared {
    fn close(&self) {
        if !self.busy.get() {
            let _ = slint::quit_event_loop();
        }
    }
}

/// 「更改」：选一个文件夹装蒜鸟
fn change_folder(shared: &Rc<Shared>) {
    if shared.busy.get() {
        return;
    }
    // 从现在要装到的文件夹的上一层开始（选的是「装到哪里面」，SuanNIAO 文件夹自己会建）
    let dir = shared.job.borrow().dir.clone();
    let start = dir.parent().map_or(dir.clone(), Path::to_path_buf);
    win::file_dialog(win::FileDialog::Folder(start, "把蒜鸟装到哪个文件夹"), |picked| {
        if let Some(picked) = picked {
            with_setup(|s| s.folder_picked(Path::new(&picked)));
        }
    });
}

impl Shared {
    fn folder_picked(&self, picked: &Path) {
        let Some(ui) = self.ui.upgrade() else { return };
        let dir = install_dir_in(picked);
        if let Err(e) = check_folder(&dir) {
            ui.set_folder_error(e.into());
            return;
        }
        let moving = self.movable && !same_place(&dir, &self.original);
        let mut job = self.job.borrow_mut();
        job.old_dir = moving.then(|| self.original.clone());
        job.dir = dir;
        ui.set_folder(job.dir.display().to_string().into());
        ui.set_folder_error("".into());
        ui.set_moving(moving);
    }
}

/// 选的文件夹 → 装到哪：选的就叫 SuanNIAO、或者里面已经有蒜鸟就直接装进去，不然在里面建一个 SuanNIAO
/// （选了 D:\ 或者「下载」，别把程序文件直接撒在里面）
fn install_dir_in(picked: &Path) -> PathBuf {
    let named = picked.file_name().is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("SuanNIAO"));
    if named || picked.join("SuanNIAO.exe").exists() {
        picked.to_path_buf()
    } else {
        picked.join("SuanNIAO")
    }
}

/// 能不能装进这个文件夹：不用管理员权限能写进去（Program Files、Windows 这些不行）
fn check_folder(dir: &Path) -> Result<(), String> {
    let Some(existing) = dir.ancestors().find(|p| p.is_dir()) else {
        return Err(format!("找不到 {}，换一个文件夹", dir.display()));
    };
    let probe = existing.join(format!(".suanniao-setup-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(_) => Err("这个文件夹要管理员权限才能写，换一个（比如 D 盘下面的）".into()),
    }
}

/// 开始装（卸载）：读界面上的选项，在别的线程里做，进度报回界面
fn start(shared: &Rc<Shared>) {
    let Some(ui) = shared.ui.upgrade() else { return };
    if shared.running.replace(true) {
        return;
    }
    let job = {
        let mut job = shared.job.borrow_mut();
        job.desktop = ui.get_desktop();
        job.remove_data = ui.get_remove_data();
        job.clone()
    };
    shared.busy.set(true);
    ui.set_busy(true);
    ui.set_progress(0.0);
    ui.set_step_text("".into());
    ui.set_page(1);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let report = |progress: f32, text: &str| {
            let (weak, text) = (weak.clone(), SharedString::from(text));
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_progress(progress);
                    ui.set_step_text(text);
                }
            });
            // 每一步都很快：停一下，看得清在做什么
            std::thread::sleep(Duration::from_millis(180));
        };
        let result = job.run(&report);
        let _ = slint::invoke_from_event_loop(move || with_setup(|s| finished(s, result)));
    });
}

fn finished(shared: &Rc<Shared>, result: Result<(), String>) {
    shared.running.set(false);
    shared.busy.set(false);
    let Some(ui) = shared.ui.upgrade() else { return };
    ui.set_busy(false);
    match result {
        Ok(()) => {
            shared.code.set(0);
            ui.set_progress(1.0);
            ui.set_page(2);
            let job = shared.job.borrow().clone();
            if job.mode == Mode::Update {
                // 打开新版，安装窗口过一会儿自己关掉
                launch(&job);
                Timer::single_shot(Duration::from_millis(1500), || with_setup(|s| s.close()));
            }
        }
        Err(e) => {
            shared.code.set(1);
            ui.set_error(e.into());
            ui.set_page(3);
        }
    }
}

/// 放到鼠标所在的屏幕中间。测试时放在屏幕外面；启动时指定了位置的（测试脚本）不动
fn place_window(ui: &SetupWindow, sandbox: bool) {
    unsafe {
        let mut startup = STARTUPINFOW::default();
        GetStartupInfoW(&mut startup);
        if startup.dwFlags.contains(STARTF_USEPOSITION) {
            return;
        }
        if sandbox {
            let right = GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN);
            ui.window().set_position(slint::PhysicalPosition::new(right - 2, 200));
            return;
        }
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return;
        }
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let scale = dpi_x as f32 / 96.0;
        let work = info.rcWork;
        let (width, height) = ((WINDOW_SIZE.0 * scale) as i32, (WINDOW_SIZE.1 * scale) as i32);
        let x = work.left + (work.right - work.left - width) / 2;
        let y = work.top + (work.bottom - work.top - height) / 2;
        ui.window().set_position(slint::PhysicalPosition::new(x.max(work.left), y.max(work.top)));
    }
}

// ───────────────────────── 安装、卸载 ─────────────────────────

impl Job {
    /// report(进度 0~1, 正在做什么)
    fn run(&self, report: &dyn Fn(f32, &str)) -> Result<(), String> {
        match self.mode {
            Mode::Uninstall => self.uninstall(report),
            _ => self.install(report),
        }
    }

    fn install(&self, report: &dyn Fn(f32, &str)) -> Result<(), String> {
        let dir = &self.dir;
        report(0.05, "正在关掉开着的蒜鸟…");
        let mut core_was_running = win::quit_running_in(dir);
        if let Some(old) = &self.old_dir {
            core_was_running |= win::quit_running_in(old);
        }
        report(0.2, "正在准备文件夹…");
        std::fs::create_dir_all(dir).map_err(|e| format!("建不了文件夹 {}：{e}", dir.display()))?;
        remove_leftovers(dir);
        let same = win::same_dir(&self.from, dir);
        for (i, name) in FILES.iter().enumerate() {
            report(0.3 + 0.2 * i as f32, &format!("正在复制程序文件（{} / {}）…", i + 1, FILES.len()));
            if !same {
                copy_in(&self.from.join(name), &dir.join(name))?;
            }
        }
        if self.mode == Mode::Install {
            report(0.75, "正在开始菜单里放「蒜鸟」…");
            self.shortcuts()?;
        }
        // 主界面自己更新时，只有它就是登记过的那份才更新登记（开发时编译出来的那份不登记）
        let registered = registered_dir(&self.places);
        if self.mode == Mode::Install || registered.as_deref().is_some_and(|r| same_place(r, dir)) {
            report(0.9, "正在系统里登记…");
            self.register()?;
        }
        if let Some(old) = &self.old_dir {
            report(0.95, "正在删掉原来位置上的蒜鸟…");
            self.remove_old(old);
        }
        if core_was_running && self.places.sandbox.is_none() {
            win::start_core_in(dir);
        }
        report(1.0, "好了");
        Ok(())
    }

    /// 开始菜单里总放一个；桌面上看选项，不要了就删掉原来放的（只删指着这个文件夹的）
    fn shortcuts(&self) -> Result<(), String> {
        let exe = self.dir.join("SuanNIAO.exe");
        let _ = std::fs::create_dir_all(&self.places.programs);
        make_shortcut(&self.places.programs.join(LINK_NAME), &exe, &self.dir).map_err(|e| format!("开始菜单里放不了「蒜鸟」：{e}"))?;
        let desktop = self.places.desktop.join(LINK_NAME);
        if self.desktop {
            let _ = std::fs::create_dir_all(&self.places.desktop);
            make_shortcut(&desktop, &exe, &self.dir).map_err(|e| format!("桌面上放不了「蒜鸟」：{e}"))?;
        } else if points_into(&desktop, &self.dir) {
            let _ = std::fs::remove_file(&desktop);
        }
        Ok(())
    }

    /// 在系统「应用」里登记（名字、版本、图标、卸载入口）。以前 Inno Setup 装的卸载程序删掉
    fn register(&self) -> Result<(), String> {
        let key = self.places.uninstall_key.as_str();
        let exe = self.dir.join("SuanNIAO.exe").display().to_string();
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key));
        }
        let t = unsafe { GetLocalTime() };
        let strings = [
            ("DisplayName", "蒜鸟".to_string()),
            ("DisplayVersion", VERSION.to_string()),
            ("Publisher", "SuanNIAO".to_string()),
            ("DisplayIcon", exe.clone()),
            ("InstallLocation", format!("{}\\", self.dir.display())),
            ("UninstallString", format!("\"{exe}\" --uninstall")),
            ("QuietUninstallString", format!("\"{exe}\" --uninstall --silent")),
            ("InstallDate", format!("{:04}{:02}{:02}", t.wYear, t.wMonth, t.wDay)),
            ("URLInfoAbout", "https://github.com/drhhhao/SuanNIAO".to_string()),
        ];
        for (name, value) in strings {
            set_string(key, name, &value)?;
        }
        let size: u64 = FILES.iter().filter_map(|file| std::fs::metadata(self.dir.join(file)).ok()).map(|m| m.len()).sum();
        let [major, minor, ..] = update::version_tuple(VERSION);
        for (name, value) in [("NoModify", 1), ("NoRepair", 1), ("EstimatedSize", (size / 1024) as u32), ("VersionMajor", major), ("VersionMinor", minor)] {
            set_dword(key, name, value)?;
        }
        for name in ["unins000.exe", "unins000.dat", "unins000.msg"] {
            let _ = std::fs::remove_file(self.dir.join(name));
        }
        Ok(())
    }

    /// 搬家以后删掉原来那份：只删蒜鸟自己的文件，文件夹空了才删；桌面快捷方式、开机自启改指新位置
    fn remove_old(&self, old: &Path) {
        for name in FILES.iter().copied().chain(["unins000.exe", "unins000.dat", "unins000.msg"]) {
            let path = old.join(name);
            if path.exists() {
                remove_file_retry(&path);
            }
        }
        remove_leftovers(old);
        let _ = std::fs::remove_dir(old);
        let desktop = self.places.desktop.join(LINK_NAME);
        if points_into(&desktop, old) {
            let _ = std::fs::remove_file(&desktop);
        }
        let marker = format!("{}\\", old.display()).to_lowercase();
        if get_string(&self.places.run_key, RUN_VALUE).is_some_and(|value| value.to_lowercase().contains(&marker)) {
            let _ = set_string(&self.places.run_key, RUN_VALUE, &format!("\"{}\"", self.dir.join("SuanNIAO-core.exe").display()));
        }
    }

    fn uninstall(&self, report: &dyn Fn(f32, &str)) -> Result<(), String> {
        let dir = &self.dir;
        report(0.1, "正在关掉开着的蒜鸟…");
        win::quit_running_in(dir);
        report(0.3, "正在删掉开始菜单和桌面上的「蒜鸟」…");
        for link in [self.places.programs.join(LINK_NAME), self.places.desktop.join(LINK_NAME)] {
            if points_into(&link, dir) {
                let _ = std::fs::remove_file(&link);
            }
        }
        report(0.5, "正在删掉程序…");
        // 自己（正在跑的 SuanNIAO.exe）删不了，退出以后再删（delete_self_later）
        let me = std::env::current_exe().ok();
        let mut stuck = Vec::new();
        for name in FILES {
            let path = dir.join(name);
            if !path.exists() || me.as_deref().is_some_and(|me| same_file(me, &path)) {
                continue;
            }
            if !remove_file_retry(&path) {
                stuck.push(name);
            }
        }
        for name in ["unins000.exe", "unins000.dat", "unins000.msg"] {
            let _ = std::fs::remove_file(dir.join(name));
        }
        remove_leftovers(dir);
        if !stuck.is_empty() {
            return Err(format!("{} 删不掉，可能还开着。关掉以后点「重试」。", stuck.join("、")));
        }
        report(0.7, "正在取消开机自启、从系统里注销…");
        // 开机自启指着这个文件夹里的后台才删（指着别处的副本不碰）
        let marker = format!("{}\\", dir.display()).to_lowercase();
        if get_string(&self.places.run_key, RUN_VALUE).is_some_and(|value| value.to_lowercase().contains(&marker)) {
            unsafe {
                let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(self.places.run_key.as_str()), &HSTRING::from(RUN_VALUE));
            }
        }
        if registered_dir(&self.places).is_none_or(|registered| same_place(&registered, dir)) {
            unsafe {
                let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(self.places.uninstall_key.as_str()));
            }
        }
        if self.remove_data {
            report(0.85, "正在删掉设置和日志…");
            let data = config::data_dir();
            // 测试时只删测试文件夹里的
            let allowed = self.places.sandbox.as_deref().is_none_or(|root| data.starts_with(root));
            if allowed && data.exists() {
                std::fs::remove_dir_all(&data).map_err(|e| format!("设置文件夹 {} 删不干净：{e}", data.display()))?;
            }
        }
        if self.places.sandbox.is_none() {
            let _ = std::fs::remove_dir_all(update::download_dir());
        }
        report(1.0, "好了");
        Ok(())
    }
}

/// 换掉一个文件：先复制成「名字.new」再改名盖过去。旧的还被占着（没关掉）就先把旧的改个名让开
fn copy_in(src: &Path, dest: &Path) -> Result<(), String> {
    let name = dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if !src.exists() {
        return Err(format!("安装包里少了 {name}，重新下载安装包试试"));
    }
    let new = dest.with_file_name(format!("{name}.new"));
    std::fs::copy(src, &new).map_err(|e| format!("放不了 {name}：{e}"))?;
    for _ in 0..10 {
        if std::fs::rename(&new, dest).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let old = dest.with_file_name(format!("{name}.old-{}", win::file_stamp()));
    if std::fs::rename(dest, &old).is_ok() && std::fs::rename(&new, dest).is_ok() {
        return Ok(());
    }
    let _ = std::fs::remove_file(&new);
    Err(format!("{name} 正被占着，换不了。关掉蒜鸟以后点「重试」。"))
}

/// 上次没装完留下的「.new」、让开的「.old-时间」
fn remove_leftovers(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if FILES.iter().any(|file| name == format!("{file}.new") || name.starts_with(&format!("{file}.old-"))) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn remove_file_retry(path: &Path) -> bool {
    for _ in 0..10 {
        if std::fs::remove_file(path).is_ok() || !path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// 卸载完退出以后删掉自己（正在跑的 exe 删不了），文件夹空了也删掉
fn delete_self_later(dir: &Path) {
    let Ok(me) = std::env::current_exe() else { return };
    if !me.parent().is_some_and(|parent| same_place(parent, dir)) {
        return;
    }
    let script = format!("/c ping -n 3 127.0.0.1 >nul & del /f /q \"{}\" & rd \"{}\"", me.display(), dir.display());
    let _ = std::process::Command::new("cmd").raw_arg(script).current_dir(std::env::temp_dir()).creation_flags(CREATE_NO_WINDOW).spawn();
}

/// 同一个文件夹：同一个地方（都在的时候按真实路径比），或者路径写法一样（不分大小写，结尾的 \ 不算）
fn same_place(a: &Path, b: &Path) -> bool {
    let text = |p: &Path| p.display().to_string().trim_end_matches('\\').to_lowercase();
    win::same_dir(a, b) || text(a) == text(b)
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

/// 登记过的安装位置（我们写的 InstallLocation，或者 Inno Setup 写的 App Path）
fn registered_dir(places: &Places) -> Option<PathBuf> {
    let text = get_string(&places.uninstall_key, "InstallLocation").or_else(|| get_string(&places.uninstall_key, "Inno Setup: App Path"))?;
    let text = text.trim().trim_end_matches('\\');
    (!text.is_empty()).then(|| PathBuf::from(text))
}

/// 文件夹里已经装着的版本：没有 SuanNIAO.exe 是 None，看不出版本是「?」
fn installed_version(dir: &Path, places: &Places) -> Option<String> {
    let exe = dir.join("SuanNIAO.exe");
    if !exe.exists() {
        return None;
    }
    let registered = || {
        registered_dir(places).filter(|r| same_place(r, dir)).and_then(|_| get_string(&places.uninstall_key, "DisplayVersion"))
    };
    Some(file_version(&exe).or_else(registered).unwrap_or_else(|| "?".into()))
}

/// exe 的文件版本 → "0.7.7"
fn file_version(path: &Path) -> Option<String> {
    unsafe {
        let name = HSTRING::from(path.as_os_str());
        let size = GetFileVersionInfoSizeW(&name, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(&name, None, size, data.as_mut_ptr().cast()).ok()?;
        let mut info: *mut c_void = std::ptr::null_mut();
        let mut len = 0u32;
        if !VerQueryValueW(data.as_ptr().cast(), w!("\\"), &mut info, &mut len).as_bool() || info.is_null() {
            return None;
        }
        let fixed = &*(info as *const VS_FIXEDFILEINFO);
        let version = format!("{}.{}.{}", fixed.dwFileVersionMS >> 16, fixed.dwFileVersionMS & 0xFFFF, fixed.dwFileVersionLS >> 16);
        (version != "0.0.0").then_some(version)
    }
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let path = SHGetKnownFolderPath(id, KF_FLAG_DONT_VERIFY, None).ok()?;
        let text = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const c_void));
        text.filter(|text| !text.is_empty()).map(PathBuf::from)
    }
}

fn make_shortcut(link: &Path, target: &Path, dir: &Path) -> windows::core::Result<()> {
    unsafe {
        let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        shell.SetPath(&HSTRING::from(target.as_os_str()))?;
        shell.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
        shell.SetDescription(w!("蒜鸟：把手机变成电脑的触控板、快捷键面板和语音输入"))?;
        shell.SetIconLocation(&HSTRING::from(target.as_os_str()), 0)?;
        shell.cast::<IPersistFile>()?.Save(&HSTRING::from(link.as_os_str()), true)
    }
}

fn shortcut_target(link: &Path) -> Option<PathBuf> {
    if !link.exists() {
        return None;
    }
    unsafe {
        let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        shell.cast::<IPersistFile>().ok()?.Load(&HSTRING::from(link.as_os_str()), STGM_READ).ok()?;
        let mut buffer = [0u16; 1024];
        shell.GetPath(&mut buffer, std::ptr::null_mut(), 0).ok()?;
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        (len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buffer[..len])))
    }
}

/// 快捷方式指着 dir 里的程序
fn points_into(link: &Path, dir: &Path) -> bool {
    shortcut_target(link).and_then(|target| target.parent().map(Path::to_path_buf)).is_some_and(|parent| same_place(&parent, dir))
}

fn get_string(key: &str, name: &str) -> Option<String> {
    let (key, name) = (HSTRING::from(key), HSTRING::from(name));
    unsafe {
        let mut size = 0u32;
        if RegGetValueW(HKEY_CURRENT_USER, &key, &name, RRF_RT_REG_SZ, None, None, Some(&mut size)) != ERROR_SUCCESS {
            return None;
        }
        let mut buffer = vec![0u16; size as usize / 2 + 1];
        let mut size = (buffer.len() * 2) as u32;
        if RegGetValueW(HKEY_CURRENT_USER, &key, &name, RRF_RT_REG_SZ, None, Some(buffer.as_mut_ptr().cast()), Some(&mut size)) != ERROR_SUCCESS {
            return None;
        }
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..len]))
    }
}

fn set_string(key: &str, name: &str, value: &str) -> Result<(), String> {
    let wide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    set_value(key, name, REG_SZ.0, wide.as_ptr().cast(), (wide.len() * 2) as u32)
}

fn set_dword(key: &str, name: &str, value: u32) -> Result<(), String> {
    set_value(key, name, REG_DWORD.0, (&value as *const u32).cast(), 4)
}

fn set_value(key: &str, name: &str, kind: u32, data: *const c_void, size: u32) -> Result<(), String> {
    let result = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(name), kind, Some(data), size) };
    if result == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("在系统里登记不了（{name}，错误 {}）", result.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let o = Options::parse(&args(&["SuanNIAO.exe", "--setup", "--update", "--dir", "C:\\x y", "--no-desktop"]));
        assert!(o.mode == Mode::Update && o.dir.as_deref() == Some(Path::new("C:\\x y")) && !o.desktop && !o.silent);
        let o = Options::parse(&args(&["SuanNIAO.exe", "--uninstall", "--silent", "--update"]));
        assert!(o.mode == Mode::Uninstall && o.silent && !o.remove_data);
    }

    #[test]
    fn install_dir() {
        assert_eq!(install_dir_in(Path::new("D:\\")), Path::new("D:\\SuanNIAO"));
        assert_eq!(install_dir_in(Path::new("D:\\Apps")), Path::new("D:\\Apps\\SuanNIAO"));
        assert_eq!(install_dir_in(Path::new("D:\\Apps\\suanniao")), Path::new("D:\\Apps\\suanniao"));
    }

    #[test]
    fn places() {
        assert!(same_place(Path::new("C:\\A\\b\\"), Path::new("c:\\a\\B")));
        assert!(!same_place(Path::new("C:\\A\\b"), Path::new("C:\\A\\bc")));
    }
}
