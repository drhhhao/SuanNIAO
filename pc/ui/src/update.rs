//! 主界面里的检查更新：「关于」里的状态和「检查更新」，侧边栏「关于」上的「新」字，有新版本时的更新窗口。
//!
//! 后台（core/src/update.rs）定时查、在托盘提示；主界面开着时自己也查，查到没提示过的新版本就弹出更新窗口。
//! 查到的结果和用户的选择都记在 update.json（suanniao-update），两边共用。
//! 「立即更新」：下载电脑端安装包、核对指纹，运行 `安装包 --update --dir 这个文件夹`，自己退出；
//! 安装程序关掉后台、换掉文件，装好以后打开新版（见 setup.rs）。

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};
use suanniao_config as config;
use suanniao_update::{self as update, Manifest, State};

use crate::{win, AppWindow};

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// 自动检查多久查一次；上次没查成的话过多久再试
const CHECK_EVERY: u64 = 6 * 3600;
const RETRY_AFTER: u64 = 30 * 60;
/// 点了「以后再说」以后多久不再自己弹出来
const LATER_QUIET: u64 = 24 * 3600;
/// 更新说明最多列几条（多的写「还有 N 条」）
const MAX_NOTES: usize = 8;

struct Updater {
    ui: slint::Weak<AppWindow>,
    state: RefCell<State>,
    checking: Cell<bool>,
    /// 正在下载：点「取消」把它设成 true
    download: RefCell<Option<Arc<AtomicBool>>>,
    /// 更新窗口里显示的是哪一版（下载、跳过、去网页用）
    shown: RefCell<Option<Manifest>>,
    timer: Timer,
}

thread_local! {
    static UPDATER: RefCell<Option<Rc<Updater>>> = const { RefCell::new(None) };
}

fn with_updater(f: impl FnOnce(&Rc<Updater>)) {
    let updater = UPDATER.with(|u| u.borrow().clone());
    if let Some(updater) = updater {
        f(&updater);
    }
}

fn toast(message: &str) {
    crate::with_ctx(|ctx| ctx.toast(message));
}

/// 接上更新相关的回调，开始定时检查。show：`SuanNIAO.exe --show-update` 打开的（托盘里点了「更新到 x」）
pub fn start(ui: &AppWindow, show: bool) {
    let updater = Rc::new(Updater {
        ui: ui.as_weak(),
        state: RefCell::new(update::load()),
        checking: Cell::new(false),
        download: RefCell::new(None),
        shown: RefCell::new(None),
        timer: Timer::default(),
    });
    UPDATER.with(|u| *u.borrow_mut() = Some(updater.clone()));
    ui.on_check_update(|| with_updater(|u| u.check(true)));
    ui.on_show_update(|| with_updater(|u| u.open_or_check()));
    ui.on_update_auto_changed(|on| with_updater(|u| u.set_auto(on)));
    ui.on_update_now(|| with_updater(|u| u.download()));
    ui.on_update_later(|| with_updater(|u| u.later()));
    ui.on_update_skip(|| with_updater(|u| u.skip()));
    ui.on_update_cancel(|| with_updater(|u| u.cancel()));
    ui.on_update_page(|| with_updater(|u| u.open_page()));
    updater.refresh();
    announce_updated();
    if show {
        updater.open_or_check();
    }
    // 打开几秒后看一眼（上次查是一小时以前就再查），之后每 10 分钟看要不要查
    Timer::single_shot(Duration::from_secs(3), || with_updater(|u| u.tick(3600)));
    updater.timer.start(TimerMode::Repeated, Duration::from_secs(600), || with_updater(|u| u.tick(CHECK_EVERY)));
    // 上次更新下载的安装包：装好了就没用了
    Timer::single_shot(Duration::from_secs(30), || {
        with_updater(|u| {
            if u.download.borrow().is_none() {
                let _ = std::fs::remove_dir_all(update::download_dir());
            }
        })
    });
}

/// 别的 `SuanNIAO.exe --show-update` 叫开着的主界面打开更新窗口
pub fn show_requested() {
    with_updater(|u| {
        u.reload();
        u.open_or_check();
    });
}

/// 刚更新完（ui.json 里记的上次打开的版本比这个旧）：提示一声，再记下这次的版本
fn announce_updated() {
    let last = crate::read_ui_state().get("lastVersion").and_then(|v| v.as_str()).map(str::to_string);
    if last.as_deref() != Some(VERSION) {
        crate::save_ui_value("lastVersion", serde_json::Value::String(VERSION.into()));
    }
    if last.is_some_and(|last| update::newer(VERSION, &last)) {
        Timer::single_shot(Duration::from_millis(800), || toast(&format!("蒜鸟已经更新到 {VERSION}")));
    }
}

impl Updater {
    fn reload(&self) {
        *self.state.borrow_mut() = update::load();
        self.refresh();
    }

    fn save(&self, change: impl FnOnce(&mut State)) {
        *self.state.borrow_mut() = update::update(change);
        self.refresh();
    }

    /// 「关于」里的状态、侧边栏的「新」字
    fn refresh(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let state = self.state.borrow();
        let checking = self.checking.get();
        let available = state.available(VERSION);
        let (status, tone) = match available {
            _ if checking => ("正在检查…".to_string(), 0),
            Some(m) if m.pc.version == state.skip => (format!("有新版本 {}（跳过了）", m.pc.version), 0),
            Some(m) => (format!("有新版本 {}", m.pc.version), 1),
            None if !state.error.is_empty() => ("没查成".to_string(), 2),
            None if state.checked == 0 => ("还没检查过".to_string(), 0),
            None => ("已经是最新版".to_string(), 4),
        };
        let note = if state.checked == 0 {
            "联网看看有没有新版本".to_string()
        } else if !state.error.is_empty() && available.is_none() {
            format!("上次检查（{}）没查成：{}", win::local_time_text(state.checked), state.error)
        } else {
            format!("上次检查：{}", win::local_time_text(state.checked))
        };
        ui.set_update_status(status.into());
        ui.set_update_tone(tone);
        ui.set_update_note(note.into());
        ui.set_update_available(available.is_some());
        ui.set_update_badge(state.offered(VERSION).is_some());
        ui.set_update_checking(checking);
        ui.set_update_auto(state.auto);
    }

    /// 有查到的新版本就打开更新窗口，没有就查一次（查到了再打开）
    fn open_or_check(self: &Rc<Self>) {
        if self.state.borrow().available(VERSION).is_some() {
            self.open();
        } else {
            self.check(true);
        }
    }

    /// 定时：自动检查开着、上次查是 every 秒以前（没查成的话 30 分钟）就再查；不用查就看要不要弹更新窗口
    fn tick(self: &Rc<Self>, every: u64) {
        self.reload();
        let state = self.state.borrow().clone();
        let since = config::now_secs().saturating_sub(state.checked);
        let due = since >= every || (!state.error.is_empty() && since >= RETRY_AFTER);
        if state.auto && due && !self.checking.get() {
            self.check(false);
        } else {
            self.maybe_show();
        }
    }

    /// 联网查一次（在别的线程里查）。manual：用户点的，查完告诉结果
    fn check(self: &Rc<Self>, manual: bool) {
        if self.checking.replace(true) {
            return;
        }
        self.refresh();
        std::thread::spawn(move || {
            let result = update::check();
            let _ = slint::invoke_from_event_loop(move || with_updater(|u| u.checked(result, manual)));
        });
    }

    fn checked(self: &Rc<Self>, result: Result<Manifest, String>, manual: bool) {
        self.checking.set(false);
        self.reload();
        match result {
            Ok(manifest) if update::newer(&manifest.pc.version, VERSION) => {
                if manual {
                    self.open();
                } else {
                    self.maybe_show();
                }
            }
            Ok(_) if manual => toast("已经是最新版"),
            Err(e) if manual => toast(&format!("没查成：{e}")),
            _ => {}
        }
    }

    /// 自己弹出更新窗口：有没跳过的新版本、这个版本还没主动提示过、一天内没点过「以后再说」、没有别的对话框开着
    fn maybe_show(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else { return };
        let state = self.state.borrow().clone();
        let Some(manifest) = state.offered(VERSION) else { return };
        let busy = ui.get_update_open()
            || ui.get_tutorial_open()
            || ui.get_confirm_open()
            || ui.get_name_open()
            || ui.get_import_open()
            || ui.get_sequences_open()
            || ui.get_grid_open();
        if busy || state.notified == manifest.pc.version || config::now_secs().saturating_sub(state.later) < LATER_QUIET {
            return;
        }
        self.open();
    }

    /// 打开更新窗口，显示查到的新版本
    fn open(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else { return };
        let Some(manifest) = self.state.borrow().available(VERSION).cloned() else { return };
        let notes: Vec<SharedString> =
            manifest.pc_notes().iter().map(|note| clean_note(note)).filter(|note| !note.is_empty()).map(SharedString::from).collect();
        ui.set_update_version(manifest.pc.version.as_str().into());
        ui.set_update_detail(detail(&manifest).into());
        ui.set_update_more(notes.len().saturating_sub(MAX_NOTES) as i32);
        ui.set_update_notes(ModelRc::new(VecModel::from(notes.into_iter().take(MAX_NOTES).collect::<Vec<_>>())));
        ui.set_update_phone(phone_hint(&manifest, &ui.get_phone_app()).into());
        if self.download.borrow().is_none() {
            ui.set_update_stage(0);
            ui.set_update_progress(0.0);
            ui.set_update_error("".into());
        }
        ui.set_update_open(true);
        let version = manifest.pc.version.clone();
        *self.shown.borrow_mut() = Some(manifest);
        self.save(|s| s.notified = version);
    }

    fn close(&self) {
        if let Some(ui) = self.ui.upgrade() {
            ui.set_update_open(false);
        }
    }

    fn set_auto(self: &Rc<Self>, on: bool) {
        self.save(|s| s.auto = on);
        if on {
            self.tick(CHECK_EVERY);
        }
    }

    /// 以后再说：关掉，一天里不再自己弹出来
    fn later(&self) {
        self.close();
        self.save(|s| s.later = config::now_secs());
    }

    /// 跳过这个版本：不再提醒（「关于」里还能更新），出了更新的版本再提醒
    fn skip(&self) {
        self.close();
        let Some(version) = self.shown.borrow().as_ref().map(|m| m.pc.version.clone()) else { return };
        self.save(|s| s.skip = version);
        toast("这个版本不再提醒，想装就在「关于」里点「查看更新」");
    }

    fn cancel(&self) {
        if let Some(cancel) = self.download.borrow().as_ref() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// 去网页下载：这次发布的网页（只认 GitHub 上这个仓库的）
    fn open_page(&self) {
        let page = self.shown.borrow().as_ref().map(|m| m.page.clone()).unwrap_or_default();
        let page = if page.starts_with("https://github.com/drhhhao/SuanNIAO/") { page } else { update::RELEASES_PAGE.to_string() };
        win::open_path(Path::new(&page));
    }

    /// 立即更新：在别的线程里下载安装包，进度显示在更新窗口里
    fn download(self: &Rc<Self>) {
        let Some(manifest) = self.shown.borrow().clone() else { return };
        let Some(ui) = self.ui.upgrade() else { return };
        if self.download.borrow().is_some() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *self.download.borrow_mut() = Some(cancel.clone());
        ui.set_update_stage(1);
        ui.set_update_progress(0.0);
        ui.set_update_progress_text("正在连接…".into());
        let mirror = manifest.mirror;
        let asset = manifest.pc;
        let dest = update::installer_path(&asset);
        std::thread::spawn(move || {
            let mut last: Option<Instant> = None;
            let result = update::download(&asset, &mirror, &dest, &cancel, |done, total| {
                if last.is_none_or(|t| t.elapsed() >= Duration::from_millis(100)) || done == total {
                    last = Some(Instant::now());
                    let _ = slint::invoke_from_event_loop(move || with_updater(|u| u.progress(done, total)));
                }
            });
            let _ = slint::invoke_from_event_loop(move || with_updater(|u| u.downloaded(result.map(|()| dest))));
        });
    }

    fn progress(&self, done: u64, total: u64) {
        let Some(ui) = self.ui.upgrade() else { return };
        if ui.get_update_stage() != 1 {
            return;
        }
        let mb = |bytes: u64| bytes as f64 / 1024.0 / 1024.0;
        if total > 0 {
            ui.set_update_progress((done as f64 / total as f64).min(1.0) as f32);
            ui.set_update_progress_text(format!("已下载 {:.1} / {:.1} MB", mb(done), mb(total)).into());
        } else {
            ui.set_update_progress_text(format!("已下载 {:.1} MB", mb(done)).into());
        }
    }

    /// 下载完了：打开安装程序，自己退出（安装程序会等这个窗口关掉、关掉后台再换文件，装好以后打开新版）
    fn downloaded(&self, result: Result<PathBuf, String>) {
        self.download.borrow_mut().take();
        let Some(ui) = self.ui.upgrade() else { return };
        let fail = |message: String| {
            ui.set_update_stage(2);
            ui.set_update_error(message.into());
        };
        let installer = match result {
            Ok(installer) => installer,
            Err(e) if e == update::CANCELLED => return ui.set_update_stage(0),
            Err(e) => return fail(format!("下载没成：{e}。可以重试，或者去网页上下载安装包。")),
        };
        ui.set_update_stage(3);
        ui.set_update_progress(1.0);
        ui.set_update_progress_text("下载好了。蒜鸟会先关掉，装好以后自己打开。".into());
        let spawned = std::process::Command::new(&installer)
            .arg("--update")
            .arg("--dir")
            .arg(win::exe_dir())
            .current_dir(installer.parent().unwrap_or(Path::new(".")))
            .spawn();
        match spawned {
            Ok(_) => {
                // 先把没保存的改动存好再退出
                Timer::single_shot(Duration::from_millis(800), || {
                    crate::with_ctx(|ctx| ctx.flush());
                    let _ = slint::quit_event_loop();
                });
            }
            Err(e) => fail(format!("安装程序打不开：{e}。可以去网页上下载安装包。")),
        }
    }
}

/// 更新说明的一条：去掉开头的「- 」和 Markdown 的反引号；雅黑没有「⋯」（手机上的菜单按钮），换成「···」
fn clean_note(note: &str) -> String {
    note.trim().trim_start_matches(['-', '*']).trim().replace('`', "").replace('⋯', "···")
}

/// 「12.3 MB · 10 月 30 日发布」
fn detail(manifest: &Manifest) -> String {
    let mut parts = Vec::new();
    if manifest.pc.size > 0 {
        parts.push(format!("{:.1} MB", manifest.pc.size as f64 / 1024.0 / 1024.0));
    }
    if let Some(secs) = config::transfer::parse_iso_utc(&manifest.published) {
        // 本地时间「2026-10-30 08:12」
        let local = win::local_time_text(secs);
        let month = local.get(5..7).and_then(|m| m.parse::<u32>().ok());
        let day = local.get(8..10).and_then(|d| d.parse::<u32>().ok());
        if let (Some(month), Some(day)) = (month, day) {
            parts.push(format!("{month} 月 {day} 日发布"));
        }
    }
    parts.join(" · ")
}

/// 手机 App 也有新版时提一句（连过手机、知道手机上是哪一版才说）
fn phone_hint(manifest: &Manifest, phone_app: &str) -> String {
    let latest = manifest.android.version.trim();
    let phone = phone_app.trim();
    if phone.is_empty() || !update::newer(latest, phone) {
        return String::new();
    }
    // 电脑上的雅黑没有「⋯」这个字，写成「···」
    format!("手机 App 也有新版 {latest}（现在是 {phone}）：手机蒜鸟里点右上角「···」→「检查更新」。")
}
