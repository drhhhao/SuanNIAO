//! 蒜鸟主界面（SuanNIAO.exe）：编辑手机布局、按键映射和设置。
//!
//! 改动 300 毫秒后自动保存到 config.json，后台发现文件变了就同步给手机。
//! 配置文件也可能被别处改（在手机上调布局时由后台写回）：每半秒看一次，被改了就重新载入；
//! 界面里还有没保存的改动时三方合并，保存前也先看一眼，不会把手机上调的布局盖掉。
//! 配置文件读不出来时不自动改它：坏文件另存一份带时间的，界面用最近一份能读的备份，顶上横幅让你选怎么办。
//! 首页每秒读一次后台写的 status.json，显示手机连接状态、同步回执。
//! 设置页可以导入遥控器映射（读本机的数据或它导出的文件，只读不写），先预览勾选，再新建一页「遥控器」。
//! 多套方案、撤销重做、复制粘贴动作、预设、已保存的序列、录按键、导出导入在 tools.rs；撤销的记录在 history.rs。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod history;
mod tools;
mod win;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use suanniao_config::actions::{app_path_problem, key_list, normalize_url, split_shortcut, MAX_CURSOR_DISTANCE, MAX_DELAY_MS};
use suanniao_config::axonkey::{self, Import};
use suanniao_config::geometry::{self, show};
use suanniao_config::profiles::IMPORTED_PROFILE_NAME;
use suanniao_config::{
    self as config, any_active, Action, Config, ConfigError, HoldMode, Module, ModuleKind, Page, PasteMode, Rect, Settings, Step,
    Trigger, ACTION_KINDS, DIRECTIONS, KEY_GROUPS, MEDIA_KEYS, MIN_SIZE, MOUSE_BUTTONS, PASTE_MODES, PROFILE_DIRECTIONS,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use windows::Win32::System::SystemInformation::GetLocalTime;

slint::include_modules!();

/// 界面上的编辑状态。
struct App {
    config: Config,
    /// 布局页正在看第几页
    page: usize,
    /// 布局页选中的模块（当前页里的下标）
    selected: Option<usize>,
    /// 映射页选中的按键（在所有按键里的下标）
    map_index: usize,
    map_trigger: Trigger,
}

/// 和配置文件同步到哪了：上次读到或写进去的内容。用来分辨文件是不是被别处改了，以及三方合并的「原样」。
struct Synced {
    text: String,
    base: Config,
    mtime: Option<SystemTime>,
}

fn config_mtime() -> Option<SystemTime> {
    std::fs::metadata(config::config_path()).and_then(|m| m.modified()).ok()
}

/// 读出来、等用户在预览里勾选的遥控器映射
struct Pending {
    import: Import,
    checked: Vec<bool>,
    /// 从哪读的（预览标题旁边显示）
    source: String,
}

/// 提示条上的按钮点了做什么
enum ToastAction {
    Undo,
    /// 在资源管理器里打开文件所在的文件夹
    Reveal(String),
}

struct Ctx {
    ui: slint::Weak<AppWindow>,
    app: RefCell<App>,
    synced: RefCell<Synced>,
    save_timer: Timer,
    dirty: Cell<bool>,
    toast_timer: Timer,
    toast_action: RefCell<Option<ToastAction>>,
    /// 今天每个按键（模块 id）按了几次，从后台的 status.json 读
    today: RefCell<BTreeMap<String, u32>>,
    /// 首页「最近动作」：新的一条插在最前面，界面上只有它淡入
    recent: Rc<VecModel<RecentItem>>,
    /// 布局画布上的模块：一直是同一个模型、只按行更新。整个换掉的话 Slint 会重建所有模块，
    /// 正按着的那个 TouchArea 也没了，按下去（顺带选中）之后就拖不动
    modules: Rc<VecModel<ModuleItem>>,
    /// 导入遥控器映射：预览开着时是读出来的映射
    pending: RefCell<Option<Pending>>,
    /// 导入遥控器映射时新建一个方案（默认）还是写进当前方案
    import_as_profile: Cell<bool>,
    /// 撤销、重做
    history: RefCell<history::History>,
    /// 确认框、起名字的对话框开着时，确定以后要做什么
    confirm: RefCell<Option<tools::Confirm>>,
    naming: RefCell<Option<tools::Naming>>,
    /// 正在录按键
    record: RefCell<tools::Record>,
    /// 映射页左边列表：搜索的字、收起来的页（页面 id）
    map_query: RefCell<String>,
    collapsed: RefCell<HashSet<String>>,
    /// 界面自己发现配置文件读不出来（顶上的横幅）
    broken: RefCell<Option<ConfigError>>,
    /// 横幅上「从最近的备份恢复」：最近一份能读的备份的名字（外层 None = 还没找过）
    backup_name: RefCell<Option<Option<String>>>,
    /// 最后一次写配置文件的时刻（Unix 秒）：算「等手机确认」用
    saved_at: Cell<u64>,
    /// 蓝牙自检：结果、上次检查的时刻、是不是正在查
    bluetooth: RefCell<Option<win::Bluetooth>>,
    bluetooth_at: Cell<Option<Instant>>,
    bluetooth_busy: Cell<bool>,
    /// 问过开机自启没有（ui.json）
    autostart_asked: Cell<bool>,
}

thread_local! {
    /// 文件对话框在别的线程里开，选完回到界面线程时靠这个找到 Ctx
    static CTX: RefCell<Option<Rc<Ctx>>> = const { RefCell::new(None) };
}

/// 在界面线程里拿到 Ctx（文件对话框、蓝牙自检选完以后用）
fn with_ctx(f: impl FnOnce(&Rc<Ctx>)) {
    let ctx = CTX.with(|c| c.borrow().clone());
    if let Some(ctx) = ctx {
        f(&ctx);
    }
}

/// 界面自己的小状态，存在 ui.json（不放进配置：不进撤销记录，也不用同步给手机）：
/// autostartAsked 问过开机自启没有，tutorialSeen 看过（或跳过）使用教程没有
fn ui_state_path() -> std::path::PathBuf {
    config::data_dir().join("ui.json")
}

fn read_ui_state() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(ui_state_path())
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| match v {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

fn ui_flag(key: &str) -> bool {
    read_ui_state().get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

/// 在 ui.json 里记下一项（别的项原样留着）
fn save_ui_flag(key: &str) {
    let mut state = read_ui_state();
    state.insert(key.to_string(), serde_json::Value::Bool(true));
    let _ = std::fs::create_dir_all(config::data_dir());
    if let Ok(text) = serde_json::to_string_pretty(&serde_json::Value::Object(state)) {
        let _ = std::fs::write(ui_state_path(), text + "\n");
    }
}

fn main() -> Result<(), slint::PlatformError> {
    if win::activate_existing_window() {
        return Ok(());
    }
    // 改名前（0.7.4 以前内部叫 PhonePad）的后台先退出，再把它的数据文件夹、开机自启换成新的
    win::stop_legacy_core();
    config::move_legacy_data_dir();
    win::move_legacy_autostart();
    config::set_local_offset_secs(win::local_offset_secs());
    replace_outdated_core();
    win::ensure_core_running();

    let ui = AppWindow::new()?;
    let lists = ui.global::<Lists>();
    lists.set_action_kinds(strings(ACTION_KINDS));
    lists.set_key_options(ModelRc::new(VecModel::from(key_options())));
    lists.set_mouse_buttons(strings(MOUSE_BUTTONS.map(|(_, name)| name)));
    lists.set_directions(strings(DIRECTIONS.map(|(_, name)| name)));
    lists.set_media_keys(strings(MEDIA_KEYS.map(|(_, name)| name)));
    lists.set_paste_modes(strings(PASTE_MODES));
    lists.set_hold_modes(strings(HoldMode::ALL.map(HoldMode::name)));
    lists.set_hold_notes(strings(HoldMode::ALL.map(HoldMode::note)));
    lists.set_profile_directions(strings(PROFILE_DIRECTIONS.map(|(_, name)| name)));
    lists.set_presets(ModelRc::new(VecModel::from(tools::preset_options())));
    lists.on_filter_keys(|query| ModelRc::new(VecModel::from(filter_keys(&query))));
    ui.set_version(env!("CARGO_PKG_VERSION").into());
    ui.set_folder(config::data_dir().display().to_string().into());
    // `SuanNIAO.exe --dark` 强制深色，方便截图检查深色下的样子
    if std::env::args().any(|a| a == "--dark") {
        ui.set_force_dark(true);
    }
    // `SuanNIAO.exe --page 2` 直接打开第几页（0 首页 … 5 关于），方便截图检查
    let mut args = std::env::args().skip_while(|a| a != "--page").skip(1);
    if let Some(page) = args.next().and_then(|p| p.parse::<i32>().ok()) {
        ui.set_nav(page.clamp(0, 5));
    }

    // 读配置：读不出来时不改文件，坏文件另存一份，先用最近的备份（和后台一样）
    let (loaded, broken, text) = match config::read_config() {
        config::Loaded::Ok(config) => {
            let text = std::fs::read_to_string(config::config_path()).unwrap_or_default();
            config::remember_good(&text);
            (config, None, text)
        }
        config::Loaded::Missing => (Config::fresh(), None, String::new()),
        config::Loaded::Broken { message, text } => {
            let saved = config::preserve_broken(&text).unwrap_or_default();
            let (config, using) = match config::latest_backup() {
                Some((config, name)) => (config, format!("备份 {name}")),
                None => (Config::fresh(), "默认布局".to_string()),
            };
            (config, Some(ConfigError { message, broken: saved, using, at: config::now_secs() }), text)
        }
    };
    let synced = Synced { text, base: loaded.clone(), mtime: config_mtime() };
    let ctx = Rc::new(Ctx {
        ui: ui.as_weak(),
        app: RefCell::new(App { config: loaded, page: 0, selected: None, map_index: 0, map_trigger: Trigger::Click }),
        synced: RefCell::new(synced),
        save_timer: Timer::default(),
        dirty: Cell::new(false),
        toast_timer: Timer::default(),
        toast_action: RefCell::new(None),
        today: RefCell::new(BTreeMap::new()),
        recent: Rc::new(VecModel::default()),
        modules: Rc::new(VecModel::default()),
        pending: RefCell::new(None),
        import_as_profile: Cell::new(true),
        history: RefCell::new(history::History::default()),
        confirm: RefCell::new(None),
        naming: RefCell::new(None),
        record: RefCell::new(tools::Record::default()),
        map_query: RefCell::new(String::new()),
        collapsed: RefCell::new(HashSet::new()),
        broken: RefCell::new(broken),
        backup_name: RefCell::new(None),
        saved_at: Cell::new(0),
        bluetooth: RefCell::new(None),
        bluetooth_at: Cell::new(None),
        bluetooth_busy: Cell::new(false),
        autostart_asked: Cell::new(ui_flag("autostartAsked")),
    });
    CTX.with(|c| *c.borrow_mut() = Some(ctx.clone()));
    ui.set_recent(ModelRc::from(ctx.recent.clone()));
    ui.set_modules(ModelRc::from(ctx.modules.clone()));
    // 教程里写的电脑名字：先用计算机名，查到蓝牙以后换成蓝牙里的名字（一般就是计算机名）
    if let Some(name) = std::env::var("COMPUTERNAME").ok().filter(|n| !n.trim().is_empty()) {
        ui.set_computer_name(name.trim().into());
    }
    ctx.refresh_all();
    wire(&ui, &ctx);
    // 第一次用：自动打开使用教程（问过开机自启、开了开机自启的是以前就在用的，不算第一次）。
    // `SuanNIAO.exe --tutorial 3` 打开教程的第几页（像从首页点开的），`--first-run` 是第一次打开时的样子，方便截图检查
    let mut args = std::env::args().skip_while(|a| a != "--tutorial").skip(1);
    if let Some(page) = args.next().and_then(|p| p.parse::<i32>().ok()) {
        ui.set_tutorial_start(page.clamp(0, 8));
        ui.set_tutorial_open(true);
    } else if std::env::args().any(|a| a == "--first-run")
        || (!ui_flag("tutorialSeen") && !ctx.autostart_asked.get() && !win::autostart_enabled())
    {
        ui.set_tutorial_first_run(true);
        ui.set_tutorial_open(true);
    }

    let status_timer = Timer::default();
    {
        let ctx = ctx.clone();
        status_timer.start(TimerMode::Repeated, Duration::from_secs(1), move || ctx.refresh_status());
    }
    // 配置文件被别处改了（手机上调了布局）就重新载入
    let watch_timer = Timer::default();
    {
        let ctx = ctx.clone();
        watch_timer.start(TimerMode::Repeated, Duration::from_millis(500), move || ctx.check_disk(false));
    }
    // 顶栏当标题栏；Windows 要窗口重画时整个重画一遍，不然没变的地方是透明的（见 win::hook_main_window）
    {
        let (repaint, hit, frame, scale) = (ui.as_weak(), ui.as_weak(), ui.as_weak(), ui.as_weak());
        win::hook_main_window(
            ui.as_weak(),
            win::MainWindow {
                redraw_all: Box::new(move || {
                    if let Some(ui) = repaint.upgrade() {
                        ui.set_repaint_all(!ui.get_repaint_all());
                    }
                }),
                title_hit: Box::new(move |x, y| hit.upgrade().map_or(0, |ui| ui.invoke_title_hit(x, y))),
                frame_changed: Box::new(move |f| {
                    if let Some(ui) = frame.upgrade() {
                        ui.set_custom_frame(f.custom);
                        ui.set_window_maximized(f.maximized);
                        ui.set_window_active(f.active);
                        ui.set_max_hover(f.max_hover);
                        ui.set_max_pressed(f.max_pressed);
                    }
                }),
                scale: Box::new(move || scale.upgrade().map_or(1.0, |ui| ui.window().scale_factor())),
            },
        );
    }
    ui.on_minimize_window(|| win::window_command(win::WindowCommand::Minimize));
    ui.on_toggle_maximize(|| win::window_command(win::WindowCommand::ToggleMaximize));
    ui.on_close_window(|| win::window_command(win::WindowCommand::Close));
    ui.run()?;
    ctx.flush();
    Ok(())
}

fn strings<'a>(items: impl IntoIterator<Item = &'a str>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(items.into_iter().map(SharedString::from).collect::<Vec<_>>()))
}

/// "0.4.0" → (0, 4, 0)；没写版本（0.3.0 以前的后台）当作 0.0.0
fn version_tuple(text: &str) -> (u32, u32, u32) {
    let mut parts = text.trim().split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

/// 开着的后台比主界面旧（刚更新完，旧的后台还在跑）：让它退出，再启动同一个文件夹里新的。
/// 旧后台看不懂新格式的配置，一定要先换掉。后台比界面新（比如还开着旧版的窗口）就不动它
fn replace_outdated_core() {
    if !win::core_running() {
        return;
    }
    let Some(status) = config::read_status() else { return };
    // 状态文件要是最近写的（后台每 5 秒写一次），才说明说的是正在跑的这个后台
    let alive = config::now_secs().saturating_sub(status.updated) <= 15;
    if alive && version_tuple(&status.version) < version_tuple(env!("CARGO_PKG_VERSION")) && win::stop_core() {
        win::start_core();
    }
}

/// 按键下拉框的行：分组标题，后面是这一组的按键（key 是在 key_list 里的位置）
fn key_options() -> Vec<KeyOption> {
    filter_keys("")
}

/// 按键下拉框里搜索：名字或按键名里有这几个字的（不分大小写），分组标题只留有结果的
fn filter_keys(query: &str) -> Vec<KeyOption> {
    let query = query.trim().to_lowercase();
    let mut rows = Vec::new();
    let mut index = 0;
    for (group, keys) in KEY_GROUPS {
        let mut hits = Vec::new();
        for (code, name) in keys {
            if query.is_empty() || code.to_lowercase().contains(&query) || name.to_lowercase().contains(&query) {
                hits.push(KeyOption { text: (*name).into(), header: false, key: index });
            }
            index += 1;
        }
        if !hits.is_empty() {
            rows.push(KeyOption { text: group.into(), header: true, key: -1 });
            rows.extend(hits);
        }
    }
    rows
}

/// 按键名在下拉框里是哪一项：（在 key_list 里的位置，在列表里第几行（含分组标题），显示名）。
/// 先按名字找；别名（Escape、LCtrl、PlayPause、中文标点……）找同一个键
fn key_entry(name: &str) -> Option<(usize, usize, &'static str)> {
    let find = |matches: &dyn Fn(&str) -> bool| {
        let mut index = 0;
        for (g, (_, keys)) in KEY_GROUPS.iter().enumerate() {
            if let Some(i) = keys.iter().position(|(code, _)| matches(code)) {
                return Some((index + i, index + i + g + 1, keys[i].1));
            }
            index += keys.len();
        }
        None
    };
    find(&|code| code.eq_ignore_ascii_case(name.trim())).or_else(|| {
        let vk = config::key_code(name)?;
        find(&|code| config::key_code(code) == Some(vk))
    })
}

/// 「按住时」在 HoldMode::ALL 里的位置（界面上的分段按钮按这个顺序）
fn hold_index(mode: HoldMode) -> i32 {
    HoldMode::ALL.iter().position(|m| *m == mode).unwrap_or(0) as i32
}

/// 把模型改成 items：只动变了的行，多了删、少了补（不整个换掉，界面上对应的元素就不会被重建）
fn sync_rows<T: Clone + PartialEq + 'static>(model: &VecModel<T>, items: Vec<T>) {
    for (i, item) in items.iter().enumerate() {
        if i >= model.row_count() {
            model.push(item.clone());
        } else if model.row_data(i).as_ref() != Some(item) {
            model.set_row_data(i, item.clone());
        }
    }
    while model.row_count() > items.len() {
        model.remove(model.row_count() - 1);
    }
}

/// 把界面上的回调接到 Ctx 的方法上。
fn wire(ui: &AppWindow, ctx: &Rc<Ctx>) {
    macro_rules! bind {
        ($on:ident, |$($arg:ident),*| $method:ident) => {{
            let ctx = ctx.clone();
            ui.$on(move |$($arg),*| ctx.$method($($arg),*));
        }};
    }
    bind!(on_start_core, | | start_core);
    bind!(on_open_folder, | | open_folder);
    bind!(on_toast_clicked, | | toast_clicked);
    bind!(on_autostart_answer, |on| autostart_answer);
    bind!(on_recheck, | | recheck);
    bind!(on_open_bluetooth_settings, | | open_bluetooth_settings);
    bind!(on_tutorial_recheck, | | tutorial_recheck);
    bind!(on_tutorial_closed, |furthest| tutorial_closed);
    bind!(on_broken_restore, | | broken_restore);
    bind!(on_broken_default, | | broken_default);
    bind!(on_select_page, |i| select_page);
    bind!(on_add_page, | | add_page);
    bind!(on_rename_page, |name| rename_page);
    bind!(on_delete_page, | | delete_page);
    bind!(on_grid_open_request, | | grid_open_request);
    bind!(on_grid_problem, |c, r| grid_problem);
    bind!(on_grid_apply, |c, r| grid_apply);
    bind!(on_grid_close, | | grid_close);
    bind!(on_select_module, |i| select_module);
    bind!(on_drag_preview, |i, e, dx, dy, free| drag_preview);
    bind!(on_place_module, |i, x, y, w, h| place_module);
    bind!(on_drag_failed, | | drag_failed);
    bind!(on_add_module, |k| add_module);
    bind!(on_delete_module, | | delete_module);
    bind!(on_kind_changed, |k| kind_changed);
    bind!(on_label_changed, |t| label_changed);
    bind!(on_rect_changed, |x, y, w, h| rect_changed);
    bind!(on_nudge, |f, d| nudge);
    bind!(on_sel_hold_changed, |i| sel_hold_changed);
    bind!(on_hold_blocked, | | hold_blocked);
    bind!(on_edit_mapping, | | edit_mapping);
    bind!(on_map_hold_changed, |i| map_hold_changed);
    bind!(on_select_button, |i| select_button);
    bind!(on_toggle_page, |i| toggle_page);
    bind!(on_search_buttons, |t| search_buttons);
    bind!(on_select_trigger, |t| select_trigger);
    bind!(on_trigger_blocked, |t| trigger_blocked);
    bind!(on_add_action, | | add_action);
    bind!(on_remove_action, |i| remove_action);
    bind!(on_move_action, |i, d| move_action);
    bind!(on_action_kind, |i, k| action_kind);
    bind!(on_action_choice, |i, c| action_choice);
    bind!(on_action_text, |i, t| action_text);
    bind!(on_action_args, |i, t| action_args);
    bind!(on_action_number, |i, n| action_number);
    bind!(on_action_flag, |i, on| action_flag);
    bind!(on_action_enabled, |i, on| action_enabled);
    bind!(on_browse, |i| browse);
    bind!(on_import_remote, |k| import_remote);
    bind!(on_import_toggle, |i| import_toggle);
    bind!(on_import_confirm, | | import_confirm);
    bind!(on_import_cancel, | | import_cancel);
    bind!(on_import_as_profile_changed, |on| import_as_profile_changed);
    bind!(on_mapping_enabled_changed, |on| mapping_enabled_changed);
    bind!(on_settings_changed, | | settings_changed);
    bind!(on_reset_settings, |i| reset_settings);
    bind!(on_autostart_changed, |on| autostart_changed);
    bind!(on_undo, | | undo);
    bind!(on_redo, | | redo);
    bind!(on_profile_selected, |i| profile_selected);
    bind!(on_profile_new, | | profile_new);
    bind!(on_profile_copy, | | profile_copy);
    bind!(on_profile_rename, | | profile_rename);
    bind!(on_profile_delete, | | profile_delete);
    bind!(on_apply_preset, |k| apply_preset);
    bind!(on_copy_actions, | | copy_actions);
    bind!(on_paste_actions, | | paste_actions);
    bind!(on_save_sequence, | | save_sequence);
    bind!(on_open_sequences, | | open_sequences);
    bind!(on_record_key, |i, t, c, a, s, m, p| record_key);
    bind!(on_sequence_save, | | save_sequence);
    bind!(on_sequence_insert, |i| sequence_insert);
    bind!(on_sequence_replace, |i| sequence_replace);
    bind!(on_sequence_rename, |i| sequence_rename);
    bind!(on_sequence_delete, |i| sequence_delete);
    bind!(on_sequences_close, | | sequences_close);
    bind!(on_confirm_accept, | | confirm_accept);
    bind!(on_confirm_cancel, | | confirm_cancel);
    bind!(on_name_accept, |t| name_accept);
    bind!(on_name_cancel, | | name_cancel);
    bind!(on_export_backup, | | export_backup);
    bind!(on_import_config, | | import_config);
    bind!(on_open_logs, | | open_logs);
}

fn kind_index(kind: ModuleKind) -> i32 {
    ModuleKind::ALL.iter().position(|k| *k == kind).unwrap_or(0) as i32
}

fn trigger_index(trigger: Trigger) -> i32 {
    Trigger::ALL.iter().position(|t| *t == trigger).unwrap_or(0) as i32
}

/// 所有按键模块的位置：（第几页，页内下标）
fn button_refs(config: &Config) -> Vec<(usize, usize)> {
    config
        .layout
        .pages
        .iter()
        .enumerate()
        .flat_map(|(pi, page)| {
            page.modules.iter().enumerate().filter(|(_, m)| m.kind == ModuleKind::Button).map(move |(mi, _)| (pi, mi))
        })
        .collect()
}

// ───────── 按键的图标 ─────────

/// 按键的图标按动作挑（不看键上写的字）：（图标名，没有合适图标时写的两三个字，颜色）。
/// 颜色：1 青柠（回车）、2 橙（删除）、3 紫（换行、切换方案）、4 绿（^C）、0 中性
fn tile(module: &Module) -> (&'static str, String, i32) {
    match module.kind {
        ModuleKind::Touchpad => ("pad", String::new(), 0),
        ModuleKind::VoiceButton => ("mic", String::new(), 1),
        ModuleKind::Voice => ("text", String::new(), 0),
        ModuleKind::Button => steps_tile(&module.behaviors.click),
    }
}

/// 一串动作的图标：看第一步会做事的
fn steps_tile(steps: &[Step]) -> (&'static str, String, i32) {
    let Some(step) = steps.iter().find(|s| s.active() && !matches!(s.action, Action::Delay { .. })) else {
        return ("btn", String::new(), 0);
    };
    match &step.action {
        Action::Key { key } => key_tile(key),
        Action::Shortcut { keys } => shortcut_tile(keys),
        Action::Mouse { .. } => ("mouse", String::new(), 0),
        Action::Wheel { .. } => ("wheel", String::new(), 0),
        Action::CursorMove { .. } => ("move", String::new(), 0),
        Action::Paste { .. } => ("text", String::new(), 0),
        Action::Delay { .. } => ("clock", String::new(), 0),
        Action::OpenApp { .. } => ("app", String::new(), 0),
        Action::OpenWebsite { .. } => ("globe", String::new(), 0),
        Action::SwitchProfile { .. } => ("layers", String::new(), 3),
        Action::Disabled | Action::Unknown(_) => ("btn", String::new(), 0),
    }
}

fn key_tile(key: &str) -> (&'static str, String, i32) {
    match config::key_code(key) {
        Some(0x0D) => ("enter", String::new(), 1),
        Some(0x08 | 0x2E) => ("del", String::new(), 2),
        Some(0x26) => ("up", String::new(), 0),
        Some(0x28) => ("down", String::new(), 0),
        Some(0x25) => ("left", String::new(), 0),
        Some(0x27) => ("right", String::new(), 0),
        Some(0x09) => ("tab", String::new(), 0),
        Some(0xAD..=0xB3) => ("music", String::new(), 0),
        Some(_) => ("", short_key(key), 0),
        None => ("btn", String::new(), 0),
    }
}

/// 写在图标块上的按键名：两三个字
fn short_key(key: &str) -> String {
    match config::key_code(key) {
        Some(0x1B) => "Esc".into(),
        Some(0x20) => "空格".into(),
        Some(0x21) => "PgUp".into(),
        Some(0x22) => "PgDn".into(),
        Some(0x24) => "Home".into(),
        Some(0x23) => "End".into(),
        Some(0x2D) => "Ins".into(),
        Some(0x5D) => "Menu".into(),
        Some(0xA2 | 0xA3) => "Ctrl".into(),
        Some(0xA0 | 0xA1) => "Shift".into(),
        Some(0xA4 | 0xA5) => "Alt".into(),
        Some(0x5B | 0x5C) => "Win".into(),
        _ => key.trim().to_uppercase().chars().take(3).collect(),
    }
}

fn shortcut_tile(keys: &[String]) -> (&'static str, String, i32) {
    let vks: Vec<u16> = keys.iter().filter_map(|k| config::key_code(k)).collect();
    if vks.len() != keys.len() || vks.is_empty() {
        return ("kbd", String::new(), 0);
    }
    let shift = |vk: u16| matches!(vk, 0xA0 | 0xA1 | 0x10);
    let ctrl = |vk: u16| matches!(vk, 0xA2 | 0xA3 | 0x11);
    match vks.as_slice() {
        [_] => key_tile(&keys[0]),
        [s, 0x0D] if shift(*s) => ("wrap", String::new(), 3),
        [s, 0x09] if shift(*s) => ("stab", String::new(), 0),
        [c, 0x08] if ctrl(*c) => ("del", String::new(), 2),
        [c, k] if ctrl(*c) && keys[1].trim().chars().count() == 1 => {
            ("", format!("^{}", keys[1].trim().to_uppercase()), if *k == 0x43 { 4 } else { 0 })
        }
        _ => ("kbd", String::new(), 0),
    }
}

/// 映射页列表右边的小字：按住时不是「长按一次」就写按住时，否则写配了哪些双击、长按
fn button_tag(module: &Module) -> &'static str {
    let b = &module.behaviors;
    match module.hold_mode() {
        HoldMode::RepeatClick => "连发",
        HoldMode::Passthrough => "直通",
        HoldMode::RepeatLongPress => "重复长按",
        HoldMode::LongPress => match (any_active(&b.double_click), any_active(&b.long_press)) {
            (true, true) => "双击 · 长按",
            (true, false) => "双击",
            (false, true) => "长按",
            _ => "",
        },
    }
}

/// 总览、首页表格的一行
fn overview_row(module: &Module, index: usize, today: u32) -> OverviewRow {
    let b = &module.behaviors;
    let hold = module.hold_mode();
    let (icon, glyph, tone) = tile(module);
    OverviewRow {
        label: module.title().into(),
        key: module.title().into(),
        click: config::summarize(&b.click).into(),
        double: config::summarize(&b.double_click).into(),
        long: config::summarize(&b.long_press).into(),
        today: today as i32,
        index: index as i32,
        hold: hold_index(hold),
        hold_name: hold.name().into(),
        icon: icon.into(),
        glyph: glyph.into(),
        tone,
        ignored: !hold.judges_other_triggers() && (any_active(&b.double_click) || any_active(&b.long_press)),
    }
}

/// 动作序列里的一步 → 映射页的一行（不合法的参数带红字提示）。profiles：一共几个方案（切换方案要两个以上）
fn action_item(step: &Step, profiles: usize) -> ActionItem {
    let position = |table: &[(&str, &str)], value: &str| table.iter().position(|(code, _)| code.eq_ignore_ascii_case(value)).unwrap_or(0) as i32;
    let mut item = ActionItem { kind: step.action.kind_index(), row: -1, enabled: step.enabled, ..Default::default() };
    match &step.action {
        Action::Key { key } if item.kind == 9 => item.choice = position(&MEDIA_KEYS, key),
        // 还没选按键：下拉框标红写「请选择按键」
        Action::Key { key } if key.is_empty() => item.problem = "还没选按键：从下拉框里选一个，或者点「录制」按一下".into(),
        Action::Key { key } => match key_entry(key) {
            Some((index, row, name)) => {
                item.choice = index as i32;
                item.row = row as i32;
                item.shown = name.into();
            }
            None => {
                item.shown = key.as_str().into();
                item.problem = format!("不认识「{key}」这个按键名，从下拉框里重新选一个").into();
            }
        },
        Action::Shortcut { keys } => {
            item.text = keys.join("+").into();
            if keys.is_empty() {
                item.problem = "还没填组合键：用 + 连接，或者点「录制」按一下".into();
            } else if let Some(bad) = keys.iter().find(|k| config::key_code(k).is_none()) {
                item.problem = format!("不认识「{bad}」：按键名和「按键」下拉框里的一样（Ctrl、Shift、Alt、Win、A、F5、Enter……），用 + 连接").into();
            }
        }
        Action::Mouse { button } => item.choice = position(&MOUSE_BUTTONS, button),
        Action::Wheel { direction } => item.choice = position(&DIRECTIONS, direction),
        Action::CursorMove { direction, distance } => {
            item.choice = position(&DIRECTIONS, direction);
            item.number = *distance as i32;
        }
        Action::Paste { text, mode } => {
            item.text = text.as_str().into();
            item.choice = match mode {
                None => 0,
                Some(PasteMode::Type) => 1,
                Some(PasteMode::Clipboard) => 2,
            };
            if text.is_empty() {
                item.problem = "还没填要输入的文字".into();
            }
        }
        Action::Delay { ms } => item.number = *ms as i32,
        Action::OpenApp { path, args, activate } => {
            item.text = path.as_str().into();
            item.args = args.as_str().into();
            item.flag = *activate;
            if let Some(problem) = app_path_problem(path) {
                item.problem = problem.into();
            }
        }
        Action::OpenWebsite { url } => {
            item.text = url.as_str().into();
            if url.trim().is_empty() {
                item.problem = "还没填网址".into();
            } else if url.trim().chars().any(char::is_whitespace) {
                item.problem = "网址里不能有空格".into();
            } else if normalize_url(url).is_none() {
                item.problem = "网址不对：只能打开 http、https 的网址".into();
            }
        }
        Action::SwitchProfile { direction, wrap } => {
            item.choice = if direction == "previous" { 1 } else { 0 };
            item.flag = *wrap;
            if profiles < 2 {
                item.problem = "现在只有一个方案，切换不会有效果：先在上面新增一个方案".into();
            }
        }
        Action::Disabled => {}
        Action::Unknown(raw) => {
            let text = raw.to_string();
            let short: String = text.chars().take(160).collect();
            item.text = if short.len() < text.len() { format!("{short}…") } else { short }.into();
        }
    }
    item
}

/// 本地日期，和后台记「今天」用量的格式一样
fn local_day() -> String {
    let t = unsafe { GetLocalTime() };
    format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay)
}

/// 首页的问候和日期，例如（"晚上好", "10月29日 星期四"）
fn greeting() -> (&'static str, String) {
    let t = unsafe { GetLocalTime() };
    let hello = match t.wHour {
        0..=4 => "夜深了",
        5..=10 => "早上好",
        11..=12 => "中午好",
        13..=17 => "下午好",
        _ => "晚上好",
    };
    let weekday = ["日", "一", "二", "三", "四", "五", "六"][t.wDayOfWeek as usize % 7];
    (hello, format!("{}月{}日 星期{weekday}", t.wMonth, t.wDay))
}

/// 「刚刚」「3 分钟前」……
fn ago(now: u64, at: u64) -> String {
    match now.saturating_sub(at) {
        0..=59 => "刚刚".into(),
        s @ 60..=3599 => format!("{} 分钟前", s / 60),
        s @ 3600..=86399 => format!("{} 小时前", s / 3600),
        s => format!("{} 天前", s / 86400),
    }
}

fn since_text(since: u64) -> String {
    let minutes = config::now_secs().saturating_sub(since) / 60;
    match minutes {
        0 => "刚刚连上".into(),
        m if m < 60 => format!("已连接 {m} 分钟"),
        m => format!("已连接 {} 小时 {} 分钟", m / 60, m % 60),
    }
}

/// 两份设置哪里不一样：（撤销合并用的记号，提示里的名字）。只看设置页上能改的
fn settings_diff(a: &Settings, b: &Settings) -> Option<(&'static str, &'static str)> {
    let (t1, t2) = (&a.touchpad, &b.touchpad);
    let (v1, v2) = (&a.voice, &b.voice);
    let (k1, k2) = (&a.triggers, &b.triggers);
    let fields: [(bool, &str, &str); 16] = [
        (t1.sensitivity != t2.sensitivity, "sensitivity", "指针速度"),
        (t1.acceleration != t2.acceleration, "acceleration", "指针加速"),
        (t1.scroll_speed != t2.scroll_speed, "scroll", "滚动速度"),
        (t1.natural_scroll != t2.natural_scroll, "natural", "自然滚动"),
        (t1.smoothing != t2.smoothing, "smoothing", "指针平滑"),
        (v1.auto_send != v2.auto_send, "auto-send", "说完自动发送"),
        (v1.delay_ms != v2.delay_ms, "delay", "停多久算说完"),
        (v1.enter_after != v2.enter_after, "enter-after", "发送后自动按回车"),
        (v1.paste != v2.paste, "paste", "文字上屏方式"),
        (k1.double_click_ms != k2.double_click_ms, "double", "双击间隔"),
        (k1.long_press_ms != k2.long_press_ms, "long", "长按时间"),
        (k1.repeat_delay_ms != k2.repeat_delay_ms, "repeat-delay", "连发延迟"),
        (k1.repeat_interval_ms != k2.repeat_interval_ms, "repeat-interval", "连发间隔"),
        (k1.long_press_repeat_delay_ms != k2.long_press_repeat_delay_ms, "long-repeat-delay", "重复长按：第二次"),
        (k1.long_press_repeat_interval_ms != k2.long_press_repeat_interval_ms, "long-repeat-interval", "重复长按：之后"),
        (k1.cursor_acceleration != k2.cursor_acceleration, "cursor", "光标加速度"),
    ];
    fields.iter().find(|(changed, ..)| *changed).map(|&(_, key, name)| (key, name))
}

/// 拖到别的模块上能不能互换：能的话返回被换走的模块（页内下标）和它们的新位置。
/// - 压到的模块都整个落在目标位置里：它们一起挪到原来的位置（相对位置不变），同尺寸互换就是这种；
/// - 只压到一个、它没有整个在里面：把它挪到原来的左上角，放得下才换。
/// 挪完以后所有模块都要在网格里、互不重叠
fn swap_plan(config: &Config, page: usize, index: usize, target: Rect) -> Option<Vec<(usize, Rect)>> {
    let modules = &config.layout.pages.get(page)?.modules;
    let me = modules.get(index)?.rect();
    let (columns, rows) = config.grid();
    if !target.inside(columns, rows) || ((target.x - me.x).abs() < geometry::EPS && (target.y - me.y).abs() < geometry::EPS) {
        return None;
    }
    let hit: Vec<usize> = modules.iter().enumerate().filter(|(i, m)| *i != index && m.rect().overlaps(&target)).map(|(i, _)| i).collect();
    if hit.is_empty() {
        return None;
    }
    let within = |outer: &Rect, inner: &Rect| {
        inner.x >= outer.x - geometry::EPS
            && inner.y >= outer.y - geometry::EPS
            && inner.right() <= outer.right() + geometry::EPS
            && inner.bottom() <= outer.bottom() + geometry::EPS
    };
    let plan: Vec<(usize, Rect)> = if hit.iter().all(|&i| within(&target, &modules[i].rect())) {
        let (dx, dy) = (me.x - target.x, me.y - target.y);
        hit.iter()
            .map(|&i| {
                let r = modules[i].rect();
                (i, Rect::new(r.x + dx, r.y + dy, r.w, r.h).tidy())
            })
            .collect()
    } else if hit.len() == 1 {
        let r = modules[hit[0]].rect();
        vec![(hit[0], Rect::new(me.x, me.y, r.w, r.h).tidy())]
    } else {
        return None;
    };
    let mut rects: Vec<Rect> = modules.iter().map(Module::rect).collect();
    rects[index] = target;
    for &(i, r) in &plan {
        rects[i] = r;
    }
    for (i, r) in rects.iter().enumerate() {
        if !r.inside(columns, rows) || rects[i + 1..].iter().any(|o| o.overlaps(r)) {
            return None;
        }
    }
    Some(plan)
}

impl Ctx {
    fn ui(&self) -> Option<AppWindow> {
        self.ui.upgrade()
    }

    // ───────── 保存与提示 ─────────

    /// 改动后 300 毫秒保存（拖滑块时不会频繁写文件）。
    fn schedule_save(self: &Rc<Self>) {
        self.dirty.set(true);
        let ctx = Rc::downgrade(self);
        self.save_timer.start(TimerMode::SingleShot, Duration::from_millis(300), move || {
            if let Some(ctx) = ctx.upgrade() {
                ctx.flush();
            }
        });
    }

    fn flush(&self) {
        if !self.dirty.get() {
            return;
        }
        // 先看文件是不是被别处改了（比如手机上刚调了布局）：改了就先合并，别把它盖掉
        self.check_disk(true);
        if !self.dirty.replace(false) {
            return;
        }
        // 新加的步骤补 id、repeat 跟着「按住时」写（旧版也读得懂连发）
        self.app.borrow_mut().config.normalize();
        let config = self.app.borrow().config.clone();
        let text = config::to_text(&config);
        match config::write_json(&config::config_path(), &text) {
            Ok(()) => {
                *self.synced.borrow_mut() = Synced { text, base: config, mtime: config_mtime() };
                self.saved_at.set(config::now_secs());
                // 读不出来的文件被现在的配置盖掉了（坏文件早就另存过）
                if let Some(error) = self.broken.borrow_mut().take() {
                    self.toast(&format!("已经用现在的配置写回去了，读不出来的那份留在 {}", error.broken));
                }
            }
            Err(e) => self.toast(&format!("保存配置失败：{e}")),
        }
    }

    /// 配置文件被别处改了（手机上调了布局、手动改了文件）就重新载入，选中的页、模块、按键都不变。
    /// 界面里还有没保存的改动时三方合并，两边的改动都留下。自己刚写进去的不算（内容一样）。
    /// 读不出来时不动界面里的配置，坏文件另存一份，顶上显示横幅。force：不看修改时间，直接比内容（保存前用）。
    fn check_disk(&self, force: bool) {
        let mtime = config_mtime();
        if !force && mtime == self.synced.borrow().mtime {
            return;
        }
        let Ok(bytes) = std::fs::read(config::config_path()) else { return };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if text == self.synced.borrow().text {
            self.synced.borrow_mut().mtime = mtime;
            return;
        }
        let theirs = match config::parse_checked(&text) {
            Ok(theirs) => theirs,
            Err(message) => {
                let broken = config::preserve_broken(&text).unwrap_or_default();
                *self.broken.borrow_mut() = Some(ConfigError { message, broken, using: "界面里现在的配置".into(), at: config::now_secs() });
                *self.backup_name.borrow_mut() = None;
                let mut synced = self.synced.borrow_mut();
                synced.text = text;
                synced.mtime = mtime;
                drop(synced);
                self.refresh_status();
                return;
            }
        };
        // 文件又能读了：横幅收起来
        if self.broken.borrow_mut().take().is_some() {
            *self.backup_name.borrow_mut() = None;
        }
        let (first, base) = {
            let synced = self.synced.borrow();
            (synced.text.is_empty(), synced.base.clone())
        };
        let mut lost = false;
        let next = if self.dirty.get() {
            let mine = self.app.borrow().config.clone();
            config::merge_configs(&base, &mine, &theirs).unwrap_or_else(|| {
                lost = true;
                theirs.clone()
            })
        } else {
            theirs.clone()
        };
        // 合并完和文件里的一样就不用再存
        self.dirty.set(self.dirty.get() && !lost && config::to_text(&next) != config::to_text(&theirs));
        let geometry_only = theirs.same_except_geometry(&base);
        self.history.borrow_mut().rebase(&base, &theirs);
        self.app.borrow_mut().config = next;
        *self.synced.borrow_mut() = Synced { text, base: theirs, mtime };
        self.refresh_all();
        if !first {
            self.toast(if lost {
                "布局在两边同时改了、对不上：以文件里的为准，刚才那一下没保存"
            } else if geometry_only {
                "手机上调整了布局，已经同步过来"
            } else {
                "配置文件在别处改过了，已重新载入"
            });
        }
    }

    fn toast(&self, message: &str) {
        self.show_toast(message, None, "");
    }

    /// 带「撤销」按钮的提示（删页面、删模块、自动新建页面……之后）
    fn toast_undo(&self, message: &str) {
        self.show_toast(message, Some(ToastAction::Undo), "撤销");
    }

    fn show_toast(&self, message: &str, action: Option<ToastAction>, label: &str) {
        let Some(ui) = self.ui() else { return };
        let long = action.is_some();
        *self.toast_action.borrow_mut() = action;
        ui.set_toast(message.into());
        ui.set_toast_action(label.into());
        let weak = self.ui.clone();
        // 带按钮的多留一会儿
        let shown = if long { Duration::from_secs(7) } else { Duration::from_secs(4) };
        self.toast_timer.start(TimerMode::SingleShot, shown, move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_toast("".into());
                ui.set_toast_action("".into());
            }
        });
    }

    fn toast_clicked(self: &Rc<Self>) {
        let action = self.toast_action.borrow_mut().take();
        if let Some(ui) = self.ui() {
            ui.set_toast("".into());
            ui.set_toast_action("".into());
        }
        match action {
            Some(ToastAction::Undo) => self.undo(),
            Some(ToastAction::Reveal(path)) => win::reveal(&path),
            None => {}
        }
    }

    /// 结构性改动（增删模块、换页面、改网格）之后：全部刷新并保存。
    fn changed(self: &Rc<Self>) {
        self.refresh_all();
        self.schedule_save();
    }

    // ───────── 刷新界面 ─────────

    fn refresh_all(&self) {
        self.refresh_profiles();
        self.refresh_layout();
        self.refresh_mapping();
        self.refresh_overview();
        self.refresh_settings();
        self.refresh_status();
        self.refresh_history();
    }

    fn module_items(page: &Page) -> Vec<ModuleItem> {
        page.modules
            .iter()
            .map(|m| ModuleItem {
                id: m.id.as_str().into(),
                kind: m.kind.code().into(),
                label: m.title().into(),
                x: m.x as f32,
                y: m.y as f32,
                w: m.w as f32,
                h: m.h as f32,
                repeat: m.repeats(),
            })
            .collect()
    }

    fn refresh_canvas(&self) {
        let (items, home, home_name) = {
            let app = self.app.borrow();
            let pages = &app.config.layout.pages;
            (Self::module_items(&pages[app.page]), Self::module_items(&pages[0]), pages[0].name.clone())
        };
        sync_rows(&self.modules, items);
        // 首页的「手机布局」卡片看第一页（主页），不跟着布局页正在看的那页走
        if let Some(ui) = self.ui() {
            ui.set_home_modules(ModelRc::new(VecModel::from(home)));
            ui.set_home_page_name(home_name.into());
        }
    }

    fn refresh_page_names(&self) {
        let Some(ui) = self.ui() else { return };
        let names: Vec<String> = self.app.borrow().config.layout.pages.iter().map(|p| p.name.clone()).collect();
        ui.set_page_names(strings(names.iter().map(String::as_str)));
    }

    fn refresh_layout(&self) {
        let Some(ui) = self.ui() else { return };
        let (page, page_name, columns, rows, selected) = {
            let mut app = self.app.borrow_mut();
            app.page = app.page.min(app.config.layout.pages.len() - 1);
            let page = &app.config.layout.pages[app.page];
            let selected = app.selected.and_then(|i| page.modules.get(i).map(|m| (i, m.clone())));
            let result = (app.page, page.name.clone(), app.config.layout.columns, app.config.layout.rows, selected.clone());
            if selected.is_none() {
                app.selected = None;
            }
            result
        };
        self.refresh_page_names();
        ui.set_page_index(page as i32);
        ui.set_page_name(page_name.into());
        ui.set_columns(columns as i32);
        ui.set_rows(rows as i32);
        self.refresh_canvas();
        match selected {
            Some((i, m)) => {
                ui.set_sel_kind(kind_index(m.kind));
                ui.set_sel_label(m.label.as_str().into());
                ui.set_sel_x(m.x as f32);
                ui.set_sel_y(m.y as f32);
                ui.set_sel_w(m.w as f32);
                ui.set_sel_h(m.h as f32);
                ui.set_sel_hold(hold_index(m.hold_mode()));
                ui.set_sel_passthrough_ok(m.passthrough_keys().is_some());
                ui.set_selected_module(i as i32);
            }
            None => ui.set_selected_module(-1),
        }
    }

    /// 映射页正在编辑的按键（第几页，页内下标）
    fn map_target(&self) -> Option<(usize, usize)> {
        let app = self.app.borrow();
        button_refs(&app.config).get(app.map_index).copied()
    }

    /// 映射页正在编辑的按键「按住时」是什么、能不能选直通
    fn map_hold(&self) -> (HoldMode, bool) {
        let app = self.app.borrow();
        match button_refs(&app.config).get(app.map_index) {
            Some(&(pi, mi)) => {
                let module = &app.config.layout.pages[pi].modules[mi];
                (module.hold_mode(), module.passthrough_keys().is_some())
            }
            None => (HoldMode::LongPress, false),
        }
    }

    fn refresh_mapping(&self) {
        let Some(ui) = self.ui() else { return };
        let (hold, passthrough_ok) = {
            let mut app = self.app.borrow_mut();
            let count = button_refs(&app.config).len();
            app.map_index = app.map_index.min(count.saturating_sub(1));
            drop(app);
            self.map_hold()
        };
        let (count, index, trigger, title, page, items) = {
            let mut app = self.app.borrow_mut();
            // 连发、直通只看单击（双击、长按不判定）
            if !hold.judges_other_triggers() {
                app.map_trigger = Trigger::Click;
            }
            let refs = button_refs(&app.config);
            let multi = app.config.layout.pages.len() > 1;
            let (title, page, items) = match refs.get(app.map_index) {
                Some(&(pi, mi)) => {
                    let module = &app.config.layout.pages[pi].modules[mi];
                    let profiles = app.config.profiles.len();
                    let items: Vec<ActionItem> = module.behaviors.get(app.map_trigger).iter().map(|s| action_item(s, profiles)).collect();
                    let page = if multi { app.config.layout.pages[pi].name.clone() } else { String::new() };
                    (format!("{} · {}", module.title(), app.map_trigger.name()), page, items)
                }
                None => (String::new(), String::new(), Vec::new()),
            };
            (refs.len(), app.map_index, trigger_index(app.map_trigger), title, page, items)
        };
        ui.set_map_hold(hold_index(hold));
        ui.set_map_passthrough_ok(passthrough_ok);
        ui.set_map_button_count(count as i32);
        ui.set_map_button_index(index as i32);
        ui.set_map_trigger_index(trigger);
        ui.set_map_title(title.into());
        ui.set_map_page(page.into());
        ui.set_map_actions(ModelRc::new(VecModel::from(items)));
        self.refresh_map_list();
    }

    /// 映射页左边的列表：页面多于一页时按页分组（可以收起）；搜索时只列名字或动作里有这几个字的
    fn refresh_map_list(&self) {
        let Some(ui) = self.ui() else { return };
        let rows = {
            let query = self.map_query.borrow().trim().to_lowercase();
            let collapsed = self.collapsed.borrow();
            let app = self.app.borrow();
            let config = &app.config;
            let multi = config.layout.pages.len() > 1;
            let mut rows = Vec::new();
            let mut index = 0i32;
            for (pi, page) in config.layout.pages.iter().enumerate() {
                let mut items = Vec::new();
                for module in page.modules.iter().filter(|m| m.kind == ModuleKind::Button) {
                    let i = index;
                    index += 1;
                    if !query.is_empty() {
                        let b = &module.behaviors;
                        let hay = format!(
                            "{} {} {} {}",
                            module.title(),
                            config::summarize(&b.click),
                            config::summarize(&b.double_click),
                            config::summarize(&b.long_press)
                        )
                        .to_lowercase();
                        if !hay.contains(&query) {
                            continue;
                        }
                    }
                    let (icon, glyph, tone) = tile(module);
                    items.push(MapRow {
                        header: false,
                        text: module.title().into(),
                        tag: button_tag(module).into(),
                        icon: icon.into(),
                        glyph: glyph.into(),
                        tone,
                        index: i,
                        collapsed: false,
                        count: 0,
                    });
                }
                if items.is_empty() {
                    continue;
                }
                let folded = query.is_empty() && collapsed.contains(&page.id);
                if multi || !query.is_empty() {
                    rows.push(MapRow {
                        header: true,
                        text: page.name.as_str().into(),
                        index: pi as i32,
                        collapsed: folded,
                        count: items.len() as i32,
                        ..Default::default()
                    });
                }
                if !folded {
                    rows.extend(items);
                }
            }
            rows
        };
        ui.set_map_list(ModelRc::new(VecModel::from(rows)));
    }

    fn refresh_overview(&self) {
        let Some(ui) = self.ui() else { return };
        let rows: Vec<OverviewRow> = {
            let app = self.app.borrow();
            let today = self.today.borrow();
            button_refs(&app.config)
                .into_iter()
                .enumerate()
                .map(|(index, (pi, mi))| {
                    let module = &app.config.layout.pages[pi].modules[mi];
                    overview_row(module, index, today.get(&module.id).copied().unwrap_or(0))
                })
                .collect()
        };
        // 首页只列今天用得最多的 5 个，次数一样的保持原来的顺序
        let mut home = rows.clone();
        home.sort_by(|a, b| b.today.cmp(&a.today));
        home.truncate(5);
        ui.set_max_today(rows.iter().map(|r| r.today).max().unwrap_or(0));
        ui.set_overview(ModelRc::new(VecModel::from(rows)));
        ui.set_home_rows(ModelRc::new(VecModel::from(home)));
        self.refresh_map_list();
    }

    /// 「最近动作」：前面多了几条就插进去（只有新的那条淡入），其余的原地更新时间
    fn refresh_recent(&self, recent: &[config::Recent]) {
        let now = config::now_secs();
        let items: Vec<RecentItem> = {
            let app = self.app.borrow();
            recent
                .iter()
                .map(|r| {
                    let (icon, glyph, tone) = app.config.find_module(&r.id).map(tile).unwrap_or(("btn", String::new(), 0));
                    RecentItem {
                        kind: r.kind.as_str().into(),
                        title: r.title.as_str().into(),
                        detail: r.detail.as_str().into(),
                        time: ago(now, r.at).into(),
                        count: r.count as i32,
                        icon: icon.into(),
                        glyph: glyph.into(),
                        tone,
                    }
                })
                .collect()
        };
        let model = &self.recent;
        let old: Vec<RecentItem> = model.iter().collect();
        if old == items {
            return;
        }
        let same = |a: &RecentItem, b: &RecentItem| a.kind == b.kind && a.title == b.title && a.detail == b.detail;
        let update_in_place = |items: &[RecentItem]| {
            for (i, item) in items.iter().enumerate() {
                if model.row_data(i).as_ref() != Some(item) {
                    model.set_row_data(i, item.clone());
                }
            }
        };
        // 条数没变、只有第一条变了（语音还在往后接）或者只是时间变了
        if old.len() == items.len() && old.iter().zip(&items).skip(1).all(|(a, b)| same(a, b)) {
            update_in_place(&items);
            return;
        }
        // 前面多了 k 条
        for k in 1..=items.len() {
            let tail = &items[k..];
            if tail.len() <= old.len() && tail.iter().zip(&old).all(|(a, b)| same(a, b)) {
                for (i, item) in items[..k].iter().enumerate() {
                    model.insert(i, item.clone());
                }
                while model.row_count() > items.len() {
                    model.remove(model.row_count() - 1);
                }
                update_in_place(&items);
                return;
            }
        }
        model.set_vec(items);
    }

    fn refresh_settings(&self) {
        let Some(ui) = self.ui() else { return };
        let (s, has_voice_box) = {
            let app = self.app.borrow();
            let has_voice_box = app.config.layout.pages.iter().flat_map(|p| p.modules.iter()).any(|m| m.kind == ModuleKind::Voice);
            (app.config.settings.clone(), has_voice_box)
        };
        ui.set_sensitivity(s.touchpad.sensitivity as f32);
        ui.set_acceleration(s.touchpad.acceleration as f32);
        ui.set_scroll_speed(s.touchpad.scroll_speed as f32);
        ui.set_natural_scroll(s.touchpad.natural_scroll);
        ui.set_smoothing(s.touchpad.smoothing);
        ui.set_auto_send(s.voice.auto_send);
        ui.set_send_delay(s.voice.delay_ms as f32 / 1000.0);
        ui.set_enter_after(s.voice.enter_after);
        ui.set_input_mode(if s.voice.paste { 1 } else { 0 });
        ui.set_double_click_ms(s.triggers.double_click_ms as i32);
        ui.set_long_press_ms(s.triggers.long_press_ms as i32);
        ui.set_repeat_delay_ms(s.triggers.repeat_delay_ms as i32);
        ui.set_repeat_interval_ms(s.triggers.repeat_interval_ms as i32);
        ui.set_long_repeat_delay_ms(s.triggers.long_press_repeat_delay_ms as i32);
        ui.set_long_repeat_interval_ms(s.triggers.long_press_repeat_interval_ms as i32);
        ui.set_cursor_acceleration(s.triggers.cursor_acceleration as f32);
        ui.set_mapping_enabled(s.enabled);
        ui.set_has_voice_box(has_voice_box);
        let autostart = win::autostart_enabled();
        ui.set_autostart(autostart);
        ui.set_ask_autostart(!self.autostart_asked.get() && !autostart);
    }

    /// 同步回执：（0 未连接 1 已同步 2 等手机确认 3 手机没有确认，文字）
    fn sync_state(&self, connected: bool, status: &config::Status) -> (i32, &'static str) {
        if !connected {
            return (0, "连上后自动同步");
        }
        let now = config::now_secs();
        let saved = self.saved_at.get();
        // 刚保存、后台还没推（后台半秒看一次文件）
        let waiting_core = saved > status.config_at && now.saturating_sub(saved) <= 5;
        if !waiting_core && status.config_rev > 0 && status.ack_rev == status.config_rev {
            return (1, "已同步");
        }
        let since = if waiting_core { saved } else { status.config_at };
        if now.saturating_sub(since) <= 5 {
            (2, "等手机确认…")
        } else {
            (3, "手机没有确认（可能是旧版 App）")
        }
    }

    fn refresh_status(&self) {
        let Some(ui) = self.ui() else { return };
        let running = win::core_running();
        let status = config::read_status().unwrap_or_default();
        let connected = running && status.connected;
        ui.set_core_running(running);
        ui.set_connected(connected);
        ui.set_device(status.device.as_str().into());
        ui.set_since_text(since_text(status.since).into());
        ui.set_memory_mb(status.memory_mb as f32);
        let battery = status.battery.filter(|_| connected);
        ui.set_battery_level(battery.map_or(-1, |b| b.level as i32));
        ui.set_battery_charging(battery.is_some_and(|b| b.charging));
        ui.set_phone_model(status.phone_model.as_str().into());
        ui.set_phone_app(status.phone_app.as_str().into());
        let (sync, sync_text) = self.sync_state(connected, &status);
        ui.set_sync_state(sync);
        ui.set_sync_text(sync_text.into());
        let (hello, date) = greeting();
        ui.set_greeting(hello.into());
        ui.set_date_text(date.into());
        // 今天的用量：后台记的不是今天（比如昨晚之后还没按过）就当作 0
        let fresh = status.day == local_day();
        ui.set_today_presses(if fresh { status.presses as i32 } else { 0 });
        ui.set_today_voice(if fresh { status.voice_chars as i32 } else { 0 });
        let counts = if fresh { status.per_button.clone() } else { BTreeMap::new() };
        if *self.today.borrow() != counts {
            *self.today.borrow_mut() = counts;
            self.refresh_overview();
        }
        self.refresh_recent(&status.recent);
        self.refresh_banner(running, &status);
        self.refresh_checks(running, connected, &status, false);
    }

    /// 配置文件读不出来的横幅：界面自己发现的，或者后台说的
    fn refresh_banner(&self, running: bool, status: &config::Status) {
        let Some(ui) = self.ui() else { return };
        let error = self.broken.borrow().clone().or_else(|| status.config_error.clone().filter(|_| running));
        match error {
            Some(e) => {
                if self.backup_name.borrow().is_none() {
                    let name = config::latest_backup().map(|(_, name)| name);
                    *self.backup_name.borrow_mut() = Some(name);
                }
                let has_backup = self.backup_name.borrow().clone().flatten().is_some();
                let saved = if e.broken.is_empty() { String::new() } else { format!("原文件另存为 {}，没有改它；", e.broken) };
                ui.set_config_broken(true);
                ui.set_has_backup(has_backup);
                ui.set_config_broken_text(format!("配置文件读不出来：{}", e.message).into());
                ui.set_config_broken_note(format!("{saved}现在用的是{}。", e.using).into());
            }
            None => {
                *self.backup_name.borrow_mut() = None;
                ui.set_config_broken(false);
            }
        }
    }

    /// 首页连接自检：没连上时每 5 秒看一次电脑蓝牙（在后台线程里查，查完回到界面线程）
    fn refresh_checks(&self, running: bool, connected: bool, status: &config::Status, force: bool) {
        let Some(ui) = self.ui() else { return };
        if (!connected || force) && !self.bluetooth_busy.get() && (force || self.bluetooth_at.get().is_none_or(|t| t.elapsed() >= Duration::from_secs(5))) {
            self.bluetooth_busy.set(true);
            std::thread::spawn(|| {
                let state = win::bluetooth();
                let _ = slint::invoke_from_event_loop(move || {
                    with_ctx(|ctx| {
                        ctx.bluetooth_busy.set(false);
                        ctx.bluetooth_at.set(Some(Instant::now()));
                        *ctx.bluetooth.borrow_mut() = Some(state);
                        ctx.refresh_status();
                    })
                });
            });
        }
        let state = self.bluetooth.borrow().clone();
        if let Some(name) = state.as_ref().map(|s| s.name.as_str()).filter(|n| !n.is_empty()) {
            ui.set_computer_name(name.into());
        }
        let (bt, bt_note) = match &state {
            _ if running && !status.bluetooth_error.is_empty() => (0, format!("后台开不了蓝牙服务（{}）：确认电脑蓝牙已打开", status.bluetooth_error)),
            Some(s) if s.radio => (1, String::new()),
            Some(_) => (0, "在电脑的「设置 → 蓝牙和其他设备」里打开蓝牙".to_string()),
            None => (-1, String::new()),
        };
        let (paired, paired_note) = match &state {
            Some(s) if !s.phones.is_empty() => (1, format!("配对过的手机：{}", s.phones.join("、"))),
            Some(s) if s.paired > 0 => (0, "配对过的设备里没有手机：在电脑的蓝牙设置里「添加设备」，选你的手机".to_string()),
            Some(_) => (0, "在电脑的蓝牙设置里「添加设备」，选你的手机".to_string()),
            None => (-1, String::new()),
        };
        ui.set_check_bluetooth(bt);
        ui.set_bluetooth_note(bt_note.into());
        ui.set_check_paired(paired);
        ui.set_paired_note(paired_note.into());
    }

    fn recheck(self: &Rc<Self>) {
        let running = win::core_running();
        let status = config::read_status().unwrap_or_default();
        self.refresh_checks(running, running && status.connected, &status, true);
        self.toast("正在重新检查…");
    }

    /// 教程里点「重新检查」：同上，不弹提示条（按钮上写着「检查中…」）
    fn tutorial_recheck(self: &Rc<Self>) {
        let running = win::core_running();
        let status = config::read_status().unwrap_or_default();
        self.refresh_checks(running, running && status.connected, &status, true);
    }

    fn open_bluetooth_settings(self: &Rc<Self>) {
        win::open_path(std::path::Path::new("ms-settings:bluetooth"));
    }

    /// 使用教程关掉了：记下看过，以后不再自动打开。翻到过开机自启那页（第 7 页）就算问过开机自启，首页不再问
    fn tutorial_closed(self: &Rc<Self>, furthest: i32) {
        save_ui_flag("tutorialSeen");
        if furthest >= 7 && !self.autostart_asked.get() {
            self.autostart_asked.set(true);
            save_ui_flag("autostartAsked");
        }
        self.refresh_settings();
    }

    // ───────── 首页 ─────────

    fn start_core(self: &Rc<Self>) {
        if !win::start_core() {
            self.toast("启动后台失败：SuanNIAO-core.exe 应该和 SuanNIAO.exe 在同一个文件夹");
        }
    }

    fn open_folder(self: &Rc<Self>) {
        let _ = std::fs::create_dir_all(config::data_dir());
        win::open_path(&config::data_dir());
    }

    /// 首页的问题：开机时自动启动后台？回答过就不再问（存在 ui.json）
    fn autostart_answer(self: &Rc<Self>, on: bool) {
        if on {
            self.autostart_changed(true);
        }
        self.autostart_asked.set(true);
        save_ui_flag("autostartAsked");
        self.refresh_settings();
        if !on {
            self.toast("好的，不开机自启。以后可以在「设置」里改");
        }
    }

    /// 配置文件读不出来：换成最近一份能读的备份（写回 config.json，坏文件早就另存过）
    fn broken_restore(self: &Rc<Self>) {
        let Some((mut backup, name)) = config::latest_backup() else {
            self.toast("没找到能用的备份");
            return;
        };
        backup.settings.autostart = self.app.borrow().config.settings.autostart;
        self.checkpoint(None, "从备份恢复");
        self.app.borrow_mut().config = backup;
        *self.broken.borrow_mut() = None;
        self.changed();
        self.flush();
        self.toast(&format!("已从备份 {name} 恢复（可以撤销）"));
    }

    fn broken_default(self: &Rc<Self>) {
        self.open_confirm(
            "用默认布局",
            "把配置换成默认的布局和「默认方案」（读不出来的文件已经另存，不会丢）。设置也会回到默认，开机自启不变。可以撤销。",
            "用默认布局",
            true,
            tools::Confirm::DefaultConfig,
        );
    }

    // ───────── 布局 ─────────

    fn select_page(self: &Rc<Self>, index: i32) {
        {
            let mut app = self.app.borrow_mut();
            app.page = (index.max(0) as usize).min(app.config.layout.pages.len() - 1);
            app.selected = None;
        }
        self.refresh_layout();
    }

    /// 新页面的名字：「第 N 页」，重名就往后数
    fn new_page_name(config: &Config) -> String {
        (config.layout.pages.len() + 1..).map(|n| format!("第 {n} 页")).find(|n| !config.layout.pages.iter().any(|p| &p.name == n)).unwrap()
    }

    fn add_page(self: &Rc<Self>) {
        let name = Self::new_page_name(&self.app.borrow().config);
        self.checkpoint(None, &format!("新建页面「{name}」"));
        {
            let mut app = self.app.borrow_mut();
            let id = app.config.new_id("page");
            app.config.layout.pages.push(Page { id, name, modules: Vec::new() });
            app.page = app.config.layout.pages.len() - 1;
            app.selected = None;
        }
        self.changed();
    }

    fn rename_page(self: &Rc<Self>, name: SharedString) {
        let page = self.app.borrow().page;
        self.checkpoint(Some(format!("page-name:{page}")), "给页面改名");
        {
            let mut app = self.app.borrow_mut();
            let page = app.page;
            app.config.layout.pages[page].name = name.to_string();
        }
        self.refresh_page_names();
        self.refresh_mapping();
        self.refresh_overview();
        self.refresh_canvas();
        self.schedule_save();
    }

    /// 删除这一页：有模块时先确认（写明有几个模块、几个方案里的映射会一起删），删完可以撤销
    fn delete_page(self: &Rc<Self>) {
        let (page_id, name, count, buttons, pages) = {
            let app = self.app.borrow();
            let page = &app.config.layout.pages[app.page];
            let buttons: Vec<String> = page.modules.iter().filter(|m| m.kind == ModuleKind::Button).map(|m| m.id.clone()).collect();
            (page.id.clone(), page.name.clone(), page.modules.len(), buttons, app.config.layout.pages.len())
        };
        if pages <= 1 {
            self.toast("至少要留一页");
            return;
        }
        if count == 0 {
            self.do_delete_page(&page_id);
            return;
        }
        let (mapped, profiles) = self.app.borrow().config.mapping_impact(&buttons);
        let mapping = if mapped > 0 {
            if profiles > 1 {
                format!("，其中 {mapped} 个按键在 {profiles} 个方案里配的动作也会一起删")
            } else {
                format!("，其中 {mapped} 个按键配的动作也会一起删")
            }
        } else {
            String::new()
        };
        self.open_confirm(
            "删除页面",
            &format!("删除「{name}」？这一页的 {count} 个模块会一起删掉{mapping}。删完可以撤销。"),
            "删除",
            true,
            tools::Confirm::DeletePage(page_id),
        );
    }

    pub(crate) fn do_delete_page(self: &Rc<Self>, page_id: &str) {
        let Some(name) = self.app.borrow().config.layout.pages.iter().find(|p| p.id == page_id).map(|p| p.name.clone()) else { return };
        self.checkpoint(None, &format!("删除页面「{name}」"));
        {
            let mut app = self.app.borrow_mut();
            if app.config.layout.pages.len() <= 1 {
                return;
            }
            if let Some(index) = app.config.layout.pages.iter().position(|p| p.id == page_id) {
                app.config.layout.pages.remove(index);
                app.page = index.min(app.config.layout.pages.len() - 1);
            }
            app.selected = None;
        }
        self.changed();
        self.toast_undo(&format!("已删除页面「{name}」"));
    }

    // ───────── 网格 ─────────

    fn grid_open_request(self: &Rc<Self>) {
        if let Some(ui) = self.ui() {
            ui.set_grid_open(true);
        }
    }

    /// 改成 columns × rows 有什么问题（空的就是没问题）
    fn grid_problem(self: &Rc<Self>, columns: i32, rows: i32) -> SharedString {
        let app = self.app.borrow();
        let (c, r) = (columns.clamp(2, 8) as f64, rows.clamp(3, 16) as f64);
        let outside = app.config.layout.pages.iter().flat_map(|p| p.modules.iter()).find(|m| {
            m.rect().right() > c + geometry::EPS || m.rect().bottom() > r + geometry::EPS
        });
        match outside {
            Some(m) => format!("「{}」会超出 {columns} 列 × {rows} 行：先把它挪进来或改小", m.title()).into(),
            None => SharedString::new(),
        }
    }

    fn grid_apply(self: &Rc<Self>, columns: i32, rows: i32) {
        if !self.grid_problem(columns, rows).is_empty() {
            return;
        }
        let (columns, rows) = (columns.clamp(2, 8) as u32, rows.clamp(3, 16) as u32);
        self.checkpoint(None, &format!("网格改成 {columns} 列 × {rows} 行"));
        {
            let mut app = self.app.borrow_mut();
            app.config.layout.columns = columns;
            app.config.layout.rows = rows;
        }
        self.grid_close();
        self.changed();
        self.toast_undo(&format!("网格改成了 {columns} 列 × {rows} 行"));
    }

    fn grid_close(self: &Rc<Self>) {
        if let Some(ui) = self.ui() {
            ui.set_grid_open(false);
        }
        self.restore_focus();
    }

    // ───────── 模块 ─────────

    fn select_module(self: &Rc<Self>, index: i32) {
        self.app.borrow_mut().selected = (index >= 0).then_some(index as usize);
        self.refresh_layout();
    }

    /// 把第 index 个模块放到 rect（单位是格）。放不下时试试和那里的模块互换；还不行就提示并还原。
    fn place(self: &Rc<Self>, index: usize, rect: Rect) {
        let rect = rect.tidy();
        let (page, title, plan, fits) = {
            let app = self.app.borrow();
            let page = app.page;
            let Some(module) = app.config.layout.pages[page].modules.get(index) else { return };
            let fits = app.config.fits(page, Some(index), rect);
            let plan = if fits { None } else { swap_plan(&app.config, page, index, rect) };
            (page, module.title(), plan, fits)
        };
        if !fits && plan.is_none() {
            self.toast(&format!("那里放不下：会和别的模块重叠、超出屏幕，或者小于 {} 格", show(MIN_SIZE)));
            self.refresh_layout();
            return;
        }
        match &plan {
            Some(plan) => {
                let other = plan.first().map(|&(i, _)| self.app.borrow().config.layout.pages[page].modules[i].title()).unwrap_or_default();
                self.checkpoint(None, &format!("把「{title}」和「{other}」互换位置"));
            }
            None => self.checkpoint(Some(format!("rect:{page}:{index}")), &format!("调整「{title}」的位置和大小")),
        }
        {
            let mut app = self.app.borrow_mut();
            let modules = &mut app.config.layout.pages[page].modules;
            modules[index].set_rect(rect);
            for &(i, r) in plan.iter().flatten() {
                modules[i].set_rect(r);
            }
            app.selected = Some(index);
        }
        self.refresh_layout();
        self.schedule_save();
        if let Some(plan) = plan {
            let names: Vec<String> = plan.iter().map(|&(i, _)| self.app.borrow().config.layout.pages[page].modules[i].title()).collect();
            self.toast_undo(&format!("「{title}」和「{}」互换了位置", names.join("」「")));
        }
    }

    fn current_rect(&self, index: usize) -> Option<Rect> {
        let app = self.app.borrow();
        app.config.layout.pages[app.page].modules.get(index).map(Module::rect)
    }

    /// 拖动中：算出松手后模块会在哪（对齐、不重叠、不出界），画布上画成虚影；拖到别的模块上能互换就画出它会去哪
    fn drag_preview(self: &Rc<Self>, index: i32, edges: i32, dx: f32, dy: f32, free: bool) -> Ghost {
        let app = self.app.borrow();
        let page = app.page;
        let Some(module) = usize::try_from(index).ok().and_then(|i| app.config.layout.pages[page].modules.get(i)) else {
            return Ghost::default();
        };
        let (columns, rows) = app.config.grid();
        let others = app.config.others(page, Some(index as usize));
        let edges = edges.clamp(0, 15) as u8;
        let (rect, ok) = geometry::drag(module.rect(), edges, dx as f64, dy as f64, !free, &others, columns, rows);
        if edges == 0 && !ok {
            if let Some(plan) = swap_plan(&app.config, page, index as usize, rect) {
                let modules = &app.config.layout.pages[page].modules;
                let moved: Vec<Area> = plan.iter().map(|(_, r)| Area { x: r.x as f32, y: r.y as f32, w: r.w as f32, h: r.h as f32 }).collect();
                let label = if plan.len() == 1 { format!("和「{}」互换", modules[plan[0].0].title()) } else { format!("和 {} 个模块互换", plan.len()) };
                return Ghost {
                    x: rect.x as f32,
                    y: rect.y as f32,
                    w: rect.w as f32,
                    h: rect.h as f32,
                    ok: true,
                    label: label.into(),
                    moved: ModelRc::new(VecModel::from(moved)),
                };
            }
        }
        let label = if edges == 0 { format!("左 {} · 上 {}", show(rect.x), show(rect.y)) } else { format!("宽 {} × 高 {}", show(rect.w), show(rect.h)) };
        Ghost { x: rect.x as f32, y: rect.y as f32, w: rect.w as f32, h: rect.h as f32, ok, label: label.into(), moved: ModelRc::default() }
    }

    fn place_module(self: &Rc<Self>, index: i32, x: f32, y: f32, w: f32, h: f32) {
        if index >= 0 {
            self.place(index as usize, Rect::new(x as f64, y as f64, w as f64, h as f64));
        }
    }

    fn drag_failed(self: &Rc<Self>) {
        self.toast("那里放不下：会和别的模块重叠，或者超出屏幕（拖到同样大小的模块上可以互换）");
    }

    /// 属性面板里直接输入的数
    fn rect_changed(self: &Rc<Self>, x: f32, y: f32, w: f32, h: f32) {
        let selected = self.app.borrow().selected;
        if let Some(index) = selected {
            self.place(index, Rect::new(x as f64, y as f64, w as f64, h as f64));
        }
    }

    /// 属性面板里的加减：走到下一个 1/4 格。field 0 左、1 上、2 宽、3 高
    fn nudge(self: &Rc<Self>, field: i32, dir: i32) {
        let selected = self.app.borrow().selected;
        let Some((index, mut rect)) = selected.and_then(|i| self.current_rect(i).map(|r| (i, r))) else { return };
        match field {
            0 => rect.x = geometry::step(rect.x, dir),
            1 => rect.y = geometry::step(rect.y, dir),
            2 => rect.w = geometry::step(rect.w, dir).max(MIN_SIZE),
            _ => rect.h = geometry::step(rect.h, dir).max(MIN_SIZE),
        }
        self.place(index, rect);
    }

    /// 添加模块：放在这一页第一个空位（放不下原来的大小就试小一点的）；这一页满了就新建一页放进去
    fn add_module(self: &Rc<Self>, kind: i32) {
        let kind = ModuleKind::ALL[kind.clamp(0, 3) as usize];
        self.checkpoint(None, &format!("添加{}", kind.name()));
        let new_page = {
            let mut app = self.app.borrow_mut();
            let columns = app.config.layout.columns;
            let (w, h) = match kind {
                ModuleKind::Button => (1.0, 1.0),
                ModuleKind::Touchpad => (2.0, 2.0),
                ModuleKind::Voice => (columns.min(4) as f64, 2.0),
                ModuleKind::VoiceButton => (columns.min(2) as f64, 1.0),
            };
            let page = app.page;
            let spot = [(w, h), (1.0, 1.0), (MIN_SIZE, MIN_SIZE)].into_iter().find_map(|(w, h)| app.config.free_spot(page, w, h).map(|(x, y)| (x, y, w, h)));
            let (page, (x, y, w, h), new_page) = match spot {
                Some(spot) => (page, spot, None),
                None => {
                    let name = Self::new_page_name(&app.config);
                    let id = app.config.new_id("page");
                    app.config.layout.pages.push(Page { id, name: name.clone(), modules: Vec::new() });
                    (app.config.layout.pages.len() - 1, (0.0, 0.0, w, h), Some(name))
                }
            };
            let id = app.config.new_id(kind.code());
            let label = if kind == ModuleKind::Button { "新按键".to_string() } else { kind.name().to_string() };
            app.config.layout.pages[page].modules.push(Module { id, kind, label, x, y, w, h, ..Default::default() });
            app.page = page;
            app.selected = Some(app.config.layout.pages[page].modules.len() - 1);
            new_page
        };
        self.changed();
        if let Some(name) = new_page {
            self.toast_undo(&format!("这一页满了，放到了新页面「{name}」"));
        }
    }

    /// 删除选中的模块：按键在哪个方案里配了动作就先确认，删完可以撤销
    fn delete_module(self: &Rc<Self>) {
        let target = {
            let app = self.app.borrow();
            let page = &app.config.layout.pages[app.page];
            app.selected.and_then(|i| page.modules.get(i)).map(|m| (page.id.clone(), m.id.clone(), m.title(), m.kind))
        };
        let Some((page_id, module_id, title, kind)) = target else { return };
        if kind == ModuleKind::Button {
            let (mapped, profiles) = self.app.borrow().config.mapping_impact(std::slice::from_ref(&module_id));
            if mapped > 0 {
                let place = if profiles > 1 { format!("它在 {profiles} 个方案里配的动作") } else { "它配的动作".to_string() };
                self.open_confirm(
                    "删除按键",
                    &format!("删除「{title}」？{place}也会一起删。删完可以撤销。"),
                    "删除",
                    true,
                    tools::Confirm::DeleteModule(page_id, module_id),
                );
                return;
            }
        }
        self.do_delete_module(&page_id, &module_id);
    }

    pub(crate) fn do_delete_module(self: &Rc<Self>, page_id: &str, module_id: &str) {
        let title = {
            let app = self.app.borrow();
            app.config.layout.pages.iter().find(|p| p.id == page_id).and_then(|p| p.modules.iter().find(|m| m.id == module_id)).map(Module::title)
        };
        let Some(title) = title else { return };
        self.checkpoint(None, &format!("删除「{title}」"));
        {
            let mut app = self.app.borrow_mut();
            if let Some(page) = app.config.layout.pages.iter_mut().find(|p| p.id == page_id) {
                page.modules.retain(|m| m.id != module_id);
            }
            app.selected = None;
        }
        self.changed();
        self.toast_undo(&format!("已删除「{title}」"));
    }

    /// 改模块类型：名字留着（按键改成触控板再改回来，还是原来的名字）
    fn kind_changed(self: &Rc<Self>, kind: i32) {
        let kind = ModuleKind::ALL[kind.clamp(0, 3) as usize];
        let title = {
            let app = self.app.borrow();
            app.selected.and_then(|i| app.config.layout.pages[app.page].modules.get(i)).map(Module::title)
        };
        let Some(title) = title else { return };
        self.checkpoint(None, &format!("把「{title}」改成{}", kind.name()));
        {
            let mut app = self.app.borrow_mut();
            let page = app.page;
            let Some(index) = app.selected else { return };
            let module = &mut app.config.layout.pages[page].modules[index];
            module.kind = kind;
            // 原来是触控板这些（名字就是类型名）改成按键：起个新名字
            if kind == ModuleKind::Button && (module.label.trim().is_empty() || ModuleKind::ALL.iter().any(|k| k.name() == module.label)) {
                module.label = "新按键".into();
            }
        }
        self.changed();
    }

    fn label_changed(self: &Rc<Self>, label: SharedString) {
        let target = {
            let app = self.app.borrow();
            (app.page, app.selected)
        };
        self.checkpoint(Some(format!("label:{}:{:?}", target.0, target.1)), "改按键上的字");
        {
            let mut app = self.app.borrow_mut();
            let page = app.page;
            let Some(index) = app.selected else { return };
            app.config.layout.pages[page].modules[index].label = label.to_string();
        }
        // 只刷新画布和列表，不动正在输入的文本框
        self.refresh_canvas();
        self.refresh_mapping();
        self.refresh_overview();
        self.schedule_save();
    }

    /// 改某个按键的「按住时」（index 是 HoldMode::ALL 里的位置）。直通要单击正好是一个按键或组合键，不行就提示、不改
    fn set_hold(self: &Rc<Self>, target: Option<(usize, usize)>, index: i32) {
        let mode = HoldMode::ALL[index.clamp(0, 3) as usize];
        let title = target.and_then(|(pi, mi)| self.app.borrow().config.layout.pages.get(pi)?.modules.get(mi).map(Module::title));
        let Some(title) = title else { return };
        self.checkpoint(None, &format!("把「{title}」的按住时改成「{}」", mode.name()));
        let changed = {
            let mut app = self.app.borrow_mut();
            let Some(module) = target.and_then(|(pi, mi)| app.config.layout.pages.get_mut(pi)?.modules.get_mut(mi)) else { return };
            if mode == HoldMode::Passthrough && module.passthrough_keys().is_none() {
                false
            } else {
                module.set_hold(mode);
                true
            }
        };
        if !changed {
            self.hold_blocked();
        }
        // 两个页面上的「按住时」都跟着变
        self.refresh_selected_hold();
        self.refresh_canvas();
        self.refresh_mapping();
        self.refresh_overview();
        if changed {
            self.schedule_save();
        }
    }

    /// 点了用不了的「直通」
    fn hold_blocked(self: &Rc<Self>) {
        self.toast("「直通」要单击正好是一个按键或组合键：先把单击设好");
    }

    /// 布局页属性面板里的「按住时」（不动别的输入框）
    fn refresh_selected_hold(&self) {
        let Some(ui) = self.ui() else { return };
        let app = self.app.borrow();
        if let Some(module) = app.selected.and_then(|i| app.config.layout.pages.get(app.page)?.modules.get(i)) {
            ui.set_sel_hold(hold_index(module.hold_mode()));
            ui.set_sel_passthrough_ok(module.passthrough_keys().is_some());
        }
    }

    /// 布局页属性面板里的「按住时」
    fn sel_hold_changed(self: &Rc<Self>, index: i32) {
        let target = {
            let app = self.app.borrow();
            app.selected.map(|i| (app.page, i))
        };
        self.set_hold(target, index);
    }

    /// 布局页「设置它的映射」：去映射页，从单击开始
    fn edit_mapping(self: &Rc<Self>) {
        let target = {
            let app = self.app.borrow();
            app.selected.and_then(|i| button_refs(&app.config).iter().position(|&r| r == (app.page, i)))
        };
        if let Some(index) = target {
            let mut app = self.app.borrow_mut();
            app.map_index = index;
            app.map_trigger = Trigger::Click;
        }
        self.refresh_mapping();
        if let Some(ui) = self.ui() {
            ui.set_nav(2);
        }
    }

    // ───────── 映射 ─────────

    /// 换了按键：按法回到「单击」
    fn select_button(self: &Rc<Self>, index: i32) {
        {
            let mut app = self.app.borrow_mut();
            app.map_index = index.max(0) as usize;
            app.map_trigger = Trigger::Click;
        }
        self.refresh_mapping();
    }

    /// 映射页列表：收起 / 展开一页
    fn toggle_page(self: &Rc<Self>, page: i32) {
        let id = self.app.borrow().config.layout.pages.get(page.max(0) as usize).map(|p| p.id.clone());
        if let Some(id) = id {
            let mut collapsed = self.collapsed.borrow_mut();
            if !collapsed.remove(&id) {
                collapsed.insert(id);
            }
        }
        self.refresh_map_list();
    }

    fn search_buttons(self: &Rc<Self>, text: SharedString) {
        *self.map_query.borrow_mut() = text.to_string();
        self.refresh_map_list();
    }

    fn select_trigger(self: &Rc<Self>, index: i32) {
        self.app.borrow_mut().map_trigger = Trigger::ALL[index.clamp(0, 2) as usize];
        self.refresh_mapping(); // 连发、直通的按键会改回单击
    }

    /// 点了灰掉的双击、长按：说明为什么用不了
    fn trigger_blocked(self: &Rc<Self>, index: i32) {
        let trigger = Trigger::ALL[index.clamp(0, 2) as usize];
        let (hold, title) = {
            let app = self.app.borrow();
            match button_refs(&app.config).get(app.map_index) {
                Some(&(pi, mi)) => {
                    let module = &app.config.layout.pages[pi].modules[mi];
                    (module.hold_mode(), module.title())
                }
                None => return,
            }
        };
        self.toast(&format!("「{title}」按住时是「{}」，{}不会生效；要用就把按住时改成「长按一次」或「重复长按」", hold.name(), trigger.name()));
    }

    /// 映射页里的「按住时」
    fn map_hold_changed(self: &Rc<Self>, index: i32) {
        let target = self.map_target();
        self.set_hold(target, index);
    }

    /// 改当前按键、当前按法的动作序列。
    fn edit_steps(self: &Rc<Self>, change: impl FnOnce(&mut Vec<Step>)) -> bool {
        let mut app = self.app.borrow_mut();
        let Some(&(pi, mi)) = button_refs(&app.config).get(app.map_index) else { return false };
        let trigger = app.map_trigger;
        change(app.config.layout.pages[pi].modules[mi].behaviors.get_mut(trigger));
        true
    }

    /// 改第 index 步
    fn edit_step(self: &Rc<Self>, index: i32, change: impl FnOnce(&mut Step)) -> bool {
        let index = index.max(0) as usize;
        let mut found = false;
        self.edit_steps(|steps| {
            if let Some(step) = steps.get_mut(index) {
                change(step);
                found = true;
            }
        });
        found
    }

    /// 动作列表结构变了（增删、换顺序、换类型）：重建列表。「按住时」的默认值、能不能直通可能跟着变。
    /// 重建以后行里原来有焦点的输入框没了，焦点还给主窗口
    fn actions_restructured(self: &Rc<Self>) {
        self.refresh_mapping();
        self.restore_focus();
        self.refresh_selected_hold();
        self.refresh_canvas();
        self.refresh_overview();
        self.schedule_save();
    }

    /// 只改了第 index 步的参数：原地更新这一行，不重建列表（免得打断正在输入的文本框）。
    /// keep 用来把文本框里正在输入的字原样放回去（存进配置的可能去掉了空格、引号）
    fn step_edited(self: &Rc<Self>, index: i32, keep: impl FnOnce(&mut ActionItem)) {
        let Some(ui) = self.ui() else { return };
        let index = index.max(0) as usize;
        let item = {
            let app = self.app.borrow();
            button_refs(&app.config).get(app.map_index).and_then(|&(pi, mi)| {
                let profiles = app.config.profiles.len();
                app.config.layout.pages[pi].modules[mi].behaviors.get(app.map_trigger).get(index).map(|s| action_item(s, profiles))
            })
        };
        let model = ui.get_map_actions();
        if let Some(mut item) = item {
            keep(&mut item);
            if index < model.row_count() {
                model.set_row_data(index, item);
            }
        }
        // 「按住时」可能跟着变：停用了一步、单击不再是一个按键（直通退回长按一次）……
        let (hold, passthrough_ok) = self.map_hold();
        if !hold.judges_other_triggers() && self.app.borrow().map_trigger != Trigger::Click {
            self.refresh_mapping();
        } else {
            ui.set_map_hold(hold_index(hold));
            ui.set_map_passthrough_ok(passthrough_ok);
        }
        self.refresh_selected_hold();
        self.refresh_canvas();
        self.refresh_overview();
        self.schedule_save();
    }

    /// 这一处的名字（撤销提示里用）：「↑ · 单击」
    fn map_place(&self) -> String {
        let app = self.app.borrow();
        button_refs(&app.config)
            .get(app.map_index)
            .map(|&(pi, mi)| format!("{} · {}", app.config.layout.pages[pi].modules[mi].title(), app.map_trigger.name()))
            .unwrap_or_default()
    }

    /// 新加的一步先空着（「请选择按键」，标红），免得忘了改就按出回车
    fn add_action(self: &Rc<Self>) {
        let label = format!("给「{}」添加动作", self.map_place());
        self.change_steps(None, &label, |steps| steps.push(Step::new(Action::default_of_kind(0))));
    }

    fn remove_action(self: &Rc<Self>, index: i32) {
        let index = index.max(0) as usize;
        let label = format!("删除「{}」的第 {} 步", self.map_place(), index + 1);
        self.change_steps(None, &label, |steps| {
            if index < steps.len() {
                steps.remove(index);
            }
        });
    }

    fn move_action(self: &Rc<Self>, index: i32, delta: i32) {
        let (from, to) = (index, index + delta);
        let label = format!("调整「{}」的动作顺序", self.map_place());
        self.change_steps(None, &label, |steps| {
            if from >= 0 && to >= 0 && (from as usize) < steps.len() && (to as usize) < steps.len() {
                steps.swap(from as usize, to as usize);
            }
        });
    }

    /// 编辑参数时的撤销记号：同一个按键、同一种按法、同一步的同一处连着改合成一步
    fn step_key(&self, field: &str, index: i32) -> Option<String> {
        let app = self.app.borrow();
        let &(pi, mi) = button_refs(&app.config).get(app.map_index)?;
        Some(format!("{field}:{}:{}:{index}", app.config.layout.pages[pi].modules[mi].id, app.map_trigger.code()))
    }

    /// 改了「切换方案」的方向、回不回绕：所有方案里这个位置的跟着改
    fn sync_shared_switch(self: &Rc<Self>, index: i32) {
        let target = {
            let app = self.app.borrow();
            button_refs(&app.config).get(app.map_index).and_then(|&(pi, mi)| {
                let module = &app.config.layout.pages[pi].modules[mi];
                let step = module.behaviors.get(app.map_trigger).get(index.max(0) as usize)?;
                matches!(step.action, Action::SwitchProfile { .. }).then(|| (module.id.clone(), app.map_trigger, step.action.clone()))
            })
        };
        if let Some((module, trigger, action)) = target {
            self.app.borrow_mut().config.update_shared_switch(&module, trigger, &action);
        }
    }

    /// 换动作类型：参数换成新类型的默认值（按键和组合键之间互换时留着键），id、启用不变
    fn action_kind(self: &Rc<Self>, index: i32, kind: i32) {
        if kind < 0 {
            return;
        }
        let index = index.max(0) as usize;
        let label = format!("改「{}」第 {} 步的类型", self.map_place(), index + 1);
        let changed = self.change_steps(None, &label, |steps| {
            let Some(step) = steps.get_mut(index) else { return };
            if step.action.kind_index() == kind {
                return;
            }
            step.action = match (&step.action, kind) {
                (Action::Key { key }, 1) if !key.is_empty() => Action::Shortcut { keys: vec![key.clone()] },
                (Action::Shortcut { keys }, 0) if keys.len() == 1 && key_entry(&keys[0]).is_some() => Action::Key { key: keys[0].clone() },
                _ => Action::default_of_kind(kind as usize),
            };
        });
        // 被确认框拦下（切换方案）时下拉框要改回去
        if !changed {
            self.refresh_mapping();
        }
    }

    /// 下拉框：按键（key_list 里的位置）、媒体键、鼠标键、方向、输入方式
    fn action_choice(self: &Rc<Self>, index: i32, choice: i32) {
        let choice = choice.max(0) as usize;
        self.checkpoint(None, &format!("改「{}」第 {} 步", self.map_place(), index + 1));
        if self.edit_step(index, |step| {
            let media = step.action.kind_index() == 9;
            match &mut step.action {
                Action::Key { key } if media => *key = MEDIA_KEYS.get(choice).map_or("MediaPlayPause", |(c, _)| *c).into(),
                Action::Key { key } => *key = key_list().get(choice).map_or("Enter", |(c, _)| *c).into(),
                Action::Mouse { button } => *button = MOUSE_BUTTONS.get(choice).map_or("left", |(c, _)| *c).into(),
                Action::Wheel { direction } | Action::CursorMove { direction, .. } => {
                    *direction = DIRECTIONS.get(choice).map_or("up", |(c, _)| *c).into()
                }
                Action::Paste { mode, .. } => {
                    *mode = match choice {
                        1 => Some(PasteMode::Type),
                        2 => Some(PasteMode::Clipboard),
                        _ => None,
                    }
                }
                Action::SwitchProfile { direction, .. } => *direction = PROFILE_DIRECTIONS.get(choice).map_or("next", |(c, _)| *c).into(),
                _ => {}
            }
        }) {
            self.sync_shared_switch(index);
            self.step_edited(index, |_| {});
        }
    }

    /// 文本框：组合键、要输入的文字、程序路径、网址
    fn action_text(self: &Rc<Self>, index: i32, text: SharedString) {
        self.checkpoint(self.step_key("text", index), &format!("改「{}」第 {} 步", self.map_place(), index + 1));
        if self.edit_step(index, |step| match &mut step.action {
            Action::Shortcut { keys } => *keys = split_shortcut(&text),
            Action::Paste { text: value, .. } => *value = text.to_string(),
            // 资源管理器「复制文件地址」会带引号
            Action::OpenApp { path, .. } => *path = text.trim().trim_matches('"').to_string(),
            Action::OpenWebsite { url } => *url = text.trim().to_string(),
            _ => {}
        }) {
            self.step_edited(index, |item| item.text = text);
        }
    }

    /// 打开应用的启动参数
    fn action_args(self: &Rc<Self>, index: i32, text: SharedString) {
        self.checkpoint(self.step_key("args", index), &format!("改「{}」第 {} 步的启动参数", self.map_place(), index + 1));
        if self.edit_step(index, |step| {
            if let Action::OpenApp { args, .. } = &mut step.action {
                *args = text.trim().to_string();
            }
        }) {
            self.step_edited(index, |item| item.args = text);
        }
    }

    /// 数字：光标移动的距离、等待的毫秒数
    fn action_number(self: &Rc<Self>, index: i32, number: i32) {
        self.checkpoint(self.step_key("number", index), &format!("改「{}」第 {} 步", self.map_place(), index + 1));
        if self.edit_step(index, |step| match &mut step.action {
            Action::CursorMove { distance, .. } => *distance = number.clamp(1, MAX_CURSOR_DISTANCE as i32) as u32,
            Action::Delay { ms } => *ms = number.clamp(0, MAX_DELAY_MS as i32) as u64,
            _ => {}
        }) {
            self.step_edited(index, |_| {});
        }
    }

    /// 打开应用：已经在运行就切到它的窗口；切换方案：回不回绕
    fn action_flag(self: &Rc<Self>, index: i32, on: bool) {
        self.checkpoint(None, &format!("改「{}」第 {} 步", self.map_place(), index + 1));
        if self.edit_step(index, |step| match &mut step.action {
            Action::OpenApp { activate, .. } => *activate = on,
            Action::SwitchProfile { wrap, .. } => *wrap = on,
            _ => {}
        }) {
            self.sync_shared_switch(index);
            self.step_edited(index, |_| {});
        }
    }

    /// 每一步的启用开关（停用的步骤执行时跳过）
    fn action_enabled(self: &Rc<Self>, index: i32, on: bool) {
        let label = format!("{}「{}」的第 {} 步", if on { "启用" } else { "停用" }, self.map_place(), index + 1);
        self.checkpoint(None, &label);
        if self.edit_step(index, |step| step.enabled = on) {
            self.step_edited(index, |_| {});
        }
    }

    /// 打开应用的「浏览…」：在别的线程里开对话框，选完填进那一步（认 id，中途换了按键也不会填错地方）
    fn browse(self: &Rc<Self>, index: i32) {
        let target = {
            let app = self.app.borrow();
            button_refs(&app.config).get(app.map_index).and_then(|&(pi, mi)| {
                let module = &app.config.layout.pages[pi].modules[mi];
                let step = module.behaviors.get(app.map_trigger).get(index.max(0) as usize)?;
                Some((module.id.clone(), app.map_trigger, step.id.clone()))
            })
        };
        let Some((module_id, trigger, step_id)) = target else { return };
        win::file_dialog(win::FileDialog::App, move |path| {
            with_ctx(|ctx| {
                if let Some(path) = path {
                    ctx.app_picked(&module_id, trigger, &step_id, path);
                }
            })
        });
    }

    fn app_picked(self: &Rc<Self>, module_id: &str, trigger: Trigger, step_id: &str, path: String) {
        let title = self.app.borrow().config.find_module(module_id).map(Module::title).unwrap_or_default();
        self.checkpoint(None, &format!("给「{title} · {}」选程序", trigger.name()));
        let found = {
            let mut app = self.app.borrow_mut();
            let module = app.config.layout.pages.iter_mut().flat_map(|p| p.modules.iter_mut()).find(|m| m.id == module_id);
            match module.and_then(|m| m.behaviors.get_mut(trigger).iter_mut().find(|s| s.id == step_id)) {
                Some(step) => {
                    if let Action::OpenApp { path: p, .. } = &mut step.action {
                        *p = path;
                    }
                    true
                }
                None => false,
            }
        };
        if found {
            self.actions_restructured();
        } else {
            self.toast("那一步已经不在了，没有填进去");
        }
    }

    // ───────── 导入遥控器映射 ─────────

    /// source 0：读本机遥控器映射软件的数据（先复制到临时文件夹再读，不碰它自己的文件）；1：选它导出的文件
    fn import_remote(self: &Rc<Self>, source: i32) {
        if source != 0 {
            win::file_dialog(win::FileDialog::RemoteMapping, |path| {
                with_ctx(|ctx| {
                    if let Some(path) = path {
                        ctx.remote_file_picked(path);
                    }
                })
            });
            return;
        }
        match config::leveldb::read_axonkey_profiles().and_then(|text| axonkey::parse(&text)) {
            Ok(import) => self.open_import(import, "本机数据".into()),
            Err(e) => self.notice("读不了本机的遥控器映射", &format!("{e}\n\n可以在遥控器映射软件里导出映射文件，再用「选择文件…」导入。")),
        }
    }

    fn remote_file_picked(self: &Rc<Self>, path: String) {
        let text = match std::fs::read(&path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => return self.notice("读不了这个文件", &e.to_string()),
        };
        match axonkey::parse(&text) {
            Ok(import) => {
                let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(path);
                self.open_import(import, name)
            }
            Err(e) => self.notice("导入不了这个文件", &format!("{e}。\n\n要选遥控器映射软件导出的映射文件或数据备份（.json）。")),
        }
    }

    /// 弹出预览：三种按法都没配的键默认不勾
    fn open_import(self: &Rc<Self>, import: Import, source: String) {
        let checked = import.keys.iter().map(|k| k.configured).collect();
        *self.pending.borrow_mut() = Some(Pending { import, checked, source });
        self.refresh_import();
    }

    fn refresh_import(&self) {
        let Some(ui) = self.ui() else { return };
        let pending = self.pending.borrow();
        let Some(p) = pending.as_ref() else {
            ui.set_import_open(false);
            return;
        };
        let rows: Vec<ImportRow> = p
            .import
            .keys
            .iter()
            .zip(&p.checked)
            .map(|(key, &checked)| {
                let b = &key.behaviors;
                let unknown = [&b.click, &b.double_click, &b.long_press].iter().flat_map(|s| s.iter()).filter(|s| matches!(s.action, Action::Unknown(_))).count();
                let mut notes = Vec::new();
                if key.filled_click {
                    notes.push("单击补成遥控器原来的键".to_string());
                }
                if unknown > 0 {
                    notes.push(format!("{unknown} 步是蒜鸟还没有的动作，照原样留着、不执行"));
                }
                if !key.configured && unknown == 0 {
                    notes.push("原来没配".into());
                }
                let (icon, glyph, tone) = steps_tile(&b.click);
                ImportRow {
                    label: key.label.into(),
                    icon: icon.into(),
                    glyph: glyph.into(),
                    tone,
                    click: config::summarize(&b.click).into(),
                    double: config::summarize(&b.double_click).into(),
                    long: config::summarize(&b.long_press).into(),
                    hold: key.hold.name().into(),
                    note: notes.join("；").into(),
                    checked,
                    configured: key.configured,
                }
            })
            .collect();
        ui.set_import_source(p.source.as_str().into());
        ui.set_import_page(axonkey::page_name(&self.app.borrow().config).into());
        ui.set_import_as_profile(self.import_as_profile.get());
        ui.set_import_profile_name(self.app.borrow().config.unique_profile_name(IMPORTED_PROFILE_NAME).into());
        ui.set_import_count(p.checked.iter().filter(|c| **c).count() as i32);
        ui.set_import_rows(ModelRc::new(VecModel::from(rows)));
        ui.set_import_open(true);
    }

    fn import_toggle(self: &Rc<Self>, index: i32) {
        if let Some(p) = self.pending.borrow_mut().as_mut() {
            if let Some(c) = p.checked.get_mut(index.max(0) as usize) {
                *c = !*c;
            }
        }
        self.refresh_import();
    }

    fn import_as_profile_changed(self: &Rc<Self>, on: bool) {
        self.import_as_profile.set(on);
        self.refresh_import();
    }

    /// 确认导入：新建一页「遥控器」（布局共用）。勾了「导入成新方案」就把这些键的映射放进新方案「遥控器映射」
    /// 并切过去，不勾就写进当前方案。可以撤销
    fn import_confirm(self: &Rc<Self>) {
        let result = {
            let pending = self.pending.borrow();
            let Some(p) = pending.as_ref() else { return };
            let app = self.app.borrow();
            p.import.build_page(&app.config, &p.checked)
        };
        match result {
            Ok(page) => {
                let (name, count) = (page.name.clone(), page.modules.len());
                self.checkpoint(None, &format!("导入遥控器映射（{count} 个键）"));
                let as_profile = self.import_as_profile.get();
                let switched = {
                    let mut app = self.app.borrow_mut();
                    let switched = if as_profile {
                        let profile = app.config.unique_profile_name(IMPORTED_PROFILE_NAME);
                        Some(app.config.add_page_as_profile(page, &profile).1)
                    } else {
                        app.config.layout.pages.push(page);
                        None
                    };
                    app.page = app.config.layout.pages.len() - 1;
                    app.selected = None;
                    switched
                };
                *self.pending.borrow_mut() = None;
                self.refresh_import();
                self.restore_focus();
                self.changed();
                if let Some(ui) = self.ui() {
                    ui.set_nav(1);
                }
                self.toast_undo(&match switched {
                    Some(profile) => format!("已导入 {count} 个键：新建了一页「{name}」和方案「{profile}」，已切过去"),
                    None => format!("已导入 {count} 个键，新建了一页「{name}」"),
                });
            }
            Err(e) => self.toast(&e),
        }
    }

    fn import_cancel(self: &Rc<Self>) {
        *self.pending.borrow_mut() = None;
        self.refresh_import();
        self.restore_focus();
    }

    // ───────── 总开关 ─────────

    /// 暂停按键 / 恢复按键：写进配置，后台读到后不再执行按键的动作（触控板、语音照常），手机上也会显示
    fn mapping_enabled_changed(self: &Rc<Self>, on: bool) {
        self.app.borrow_mut().config.settings.enabled = on;
        if let Some(ui) = self.ui() {
            ui.set_mapping_enabled(on);
        }
        self.schedule_save();
        self.toast(if on { "已恢复按键" } else { "已暂停按键：手机上的按键不执行，触控板和语音照常" });
    }

    // ───────── 设置 ─────────

    /// 设置页改了一项：记进撤销（拖滑块这类连着的改动合成一步），300 毫秒后保存
    fn settings_changed(self: &Rc<Self>) {
        let Some(ui) = self.ui() else { return };
        let mut next = self.app.borrow().config.settings.clone();
        {
            let s = &mut next;
            s.touchpad.sensitivity = (ui.get_sensitivity() as f64 * 10.0).round() / 10.0;
            s.touchpad.acceleration = (ui.get_acceleration() as f64 * 10.0).round() / 10.0;
            s.touchpad.scroll_speed = (ui.get_scroll_speed() as f64 * 10.0).round() / 10.0;
            s.touchpad.natural_scroll = ui.get_natural_scroll();
            s.touchpad.smoothing = ui.get_smoothing();
            s.voice.auto_send = ui.get_auto_send();
            s.voice.delay_ms = (ui.get_send_delay() * 1000.0).round().max(300.0) as u32;
            s.voice.enter_after = ui.get_enter_after();
            s.voice.paste = ui.get_input_mode() == 1;
            s.triggers.double_click_ms = ui.get_double_click_ms().max(100) as u32;
            s.triggers.long_press_ms = ui.get_long_press_ms().max(200) as u32;
            s.triggers.repeat_delay_ms = ui.get_repeat_delay_ms().clamp(150, 1000) as u32;
            s.triggers.repeat_interval_ms = ui.get_repeat_interval_ms().clamp(20, 200) as u32;
            s.triggers.long_press_repeat_delay_ms = ui.get_long_repeat_delay_ms().clamp(100, 1000) as u32;
            s.triggers.long_press_repeat_interval_ms = ui.get_long_repeat_interval_ms().clamp(30, 500) as u32;
            s.triggers.cursor_acceleration = ui.get_cursor_acceleration().round().clamp(0.0, 100.0) as u32;
        }
        let diff = settings_diff(&self.app.borrow().config.settings, &next);
        let Some((key, name)) = diff else { return };
        self.checkpoint(Some(format!("settings:{key}")), &format!("改{name}"));
        self.app.borrow_mut().config.settings = next;
        self.schedule_save();
    }

    /// 设置卡片的「恢复默认」：0 触控板、1 语音输入、2 按键判定（可以撤销）
    fn reset_settings(self: &Rc<Self>, section: i32) {
        let name = match section {
            0 => "触控板",
            1 => "语音输入",
            _ => "按键判定",
        };
        let defaults = Settings::default();
        let same = {
            let s = &self.app.borrow().config.settings;
            match section {
                0 => serde_json::to_value(&s.touchpad).ok() == serde_json::to_value(&defaults.touchpad).ok(),
                1 => serde_json::to_value(&s.voice).ok() == serde_json::to_value(&defaults.voice).ok(),
                _ => serde_json::to_value(&s.triggers).ok() == serde_json::to_value(&defaults.triggers).ok(),
            }
        };
        if same {
            self.toast(&format!("「{name}」已经是默认的了"));
            return;
        }
        self.checkpoint(None, &format!("恢复默认：{name}"));
        {
            let mut app = self.app.borrow_mut();
            let s = &mut app.config.settings;
            match section {
                0 => s.touchpad = defaults.touchpad,
                1 => s.voice = defaults.voice,
                _ => s.triggers = defaults.triggers,
            }
        }
        self.refresh_settings();
        self.schedule_save();
        self.toast_undo(&format!("「{name}」恢复默认了"));
    }

    fn autostart_changed(self: &Rc<Self>, on: bool) {
        if win::set_autostart(on) {
            self.app.borrow_mut().config.settings.autostart = on;
            self.schedule_save();
            self.toast(if on { "已设置开机自动启动后台" } else { "已取消开机自启" });
        } else {
            self.toast("修改开机自启失败");
        }
        if let Some(ui) = self.ui() {
            ui.set_autostart(win::autostart_enabled());
        }
    }
}
