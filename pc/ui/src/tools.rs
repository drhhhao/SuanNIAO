//! 映射和设置的进阶功能：方案、撤销重做、对话框、复制粘贴动作、一键预设、已保存的动作序列、录按键、
//! 导出导入备份、打开日志目录。都是 Ctx 的方法，界面上的回调在 main.rs 的 wire() 里接。

use std::rc::Rc;

use suanniao_config::actions;
use suanniao_config::presets::presets;
use suanniao_config::{sequence, transfer, SavedSequence};
use slint::platform::Key;

use super::*;

/// 确认框点了确定以后要做的事
pub enum Confirm {
    /// 只是告诉一声（比如导入不了的原因）：只有一个「知道了」
    Nothing,
    DeleteProfile(String),
    /// 所有方案里这个按键的这个按法都换成只有这一步「切换方案」
    SharedSwitch { module: String, trigger: Trigger, action: Action },
    /// 删掉所有方案里这个位置的「切换方案」，当前按法换成 steps
    RemoveSwitch { module: String, trigger: Trigger, steps: Vec<Step> },
    ImportConfig(Box<Config>),
    /// 删除页面（页面 id）
    DeletePage(String),
    /// 删除模块（页面 id，模块 id）
    DeleteModule(String, String),
    /// 配置文件读不出来时「用默认布局」
    DefaultConfig,
}

/// 起名字的对话框是给什么起的
pub enum Naming {
    NewProfile,
    CopyProfile,
    RenameProfile,
    SaveSequence,
    RenameSequence(usize),
}

/// 正在录按键：按着的修饰键（按下的先后）
#[derive(Default)]
pub struct Record {
    held: Vec<&'static str>,
}

/// 预设菜单的行：分组标题 + 预设（key 是在 presets() 里的位置）
pub fn preset_options() -> Vec<KeyOption> {
    let mut rows = Vec::new();
    let mut group = "";
    for (i, preset) in presets().iter().enumerate() {
        if preset.group != group {
            group = preset.group;
            rows.push(KeyOption { text: group.into(), header: true, key: -1 });
        }
        rows.push(KeyOption { text: preset.name.into(), header: false, key: i as i32 });
    }
    rows
}

fn is_switch(step: &Step) -> bool {
    matches!(step.action, Action::SwitchProfile { .. })
}

/// 修饰键的名字（Slint 给的按键分不出左右：右边的修饰键也认成左边的）
fn modifier_name(c: char) -> Option<&'static str> {
    let table: [(Key, &str); 8] = [
        (Key::Control, "Ctrl"),
        (Key::ControlR, "RCtrl"),
        (Key::Shift, "Shift"),
        (Key::ShiftR, "RShift"),
        (Key::Alt, "Alt"),
        (Key::AltGr, "RAlt"),
        (Key::Meta, "Win"),
        (Key::MetaR, "RWin"),
    ];
    table.iter().find(|(k, _)| char::from(k.clone()) == c).map(|(_, n)| *n)
}

/// 修饰键排的顺序：Ctrl、Alt、Shift、Win
fn modifier_rank(name: &str) -> usize {
    ["Ctrl", "RCtrl", "Alt", "RAlt", "Shift", "RShift", "Win", "RWin"].iter().position(|m| *m == name).unwrap_or(9) / 2
}

/// 普通键的名字：字母大写、Shift 出来的标点换回原来的键，特殊键按蒜鸟的按键名
fn key_name(c: char) -> Option<String> {
    let special: [(Key, &str); 17] = [
        (Key::Return, "Enter"),
        (Key::Tab, "Tab"),
        (Key::Backtab, "Tab"),
        (Key::Backspace, "Backspace"),
        (Key::Delete, "Delete"),
        (Key::Insert, "Insert"),
        (Key::Home, "Home"),
        (Key::End, "End"),
        (Key::PageUp, "PageUp"),
        (Key::PageDown, "PageDown"),
        (Key::UpArrow, "Up"),
        (Key::DownArrow, "Down"),
        (Key::LeftArrow, "Left"),
        (Key::RightArrow, "Right"),
        (Key::Space, "Space"),
        (Key::Menu, "Apps"),
        (Key::Escape, "Esc"),
    ];
    if let Some((_, name)) = special.iter().find(|(k, _)| char::from(k.clone()) == c) {
        return Some(name.to_string());
    }
    let functions = [
        Key::F1, Key::F2, Key::F3, Key::F4, Key::F5, Key::F6, Key::F7, Key::F8, Key::F9, Key::F10, Key::F11, Key::F12,
        Key::F13, Key::F14, Key::F15, Key::F16, Key::F17, Key::F18, Key::F19, Key::F20, Key::F21, Key::F22, Key::F23, Key::F24,
    ];
    if let Some(n) = functions.iter().position(|k| char::from(k.clone()) == c) {
        return Some(format!("F{}", n + 1));
    }
    // 按着 Ctrl 时有的系统给的是控制字符（Ctrl+C = 0x03）
    if ('\u{1}'..='\u{1a}').contains(&c) {
        return Some(((b'A' + c as u8 - 1) as char).to_string());
    }
    if c.is_ascii_alphanumeric() {
        return Some(c.to_ascii_uppercase().to_string());
    }
    let shifted = "!@#$%^&*()_+{}|:\"<>?~";
    let base = "1234567890-=[]\\;',./`";
    if let Some(i) = shifted.find(c) {
        return base.chars().nth(shifted[..i].chars().count()).map(|b| b.to_string());
    }
    base.contains(c).then(|| c.to_string())
}

impl Ctx {
    // ───────── 撤销、重做 ─────────

    /// 改配置之前调：记下改之前的样子和这一步做了什么。key 是改的哪一处（同一处连着的改动合成一步）
    pub(crate) fn checkpoint(&self, key: Option<String>, label: &str) {
        let before = self.app.borrow().config.clone();
        self.history.borrow_mut().record(&before, key.as_deref(), label);
        self.refresh_history();
    }

    pub(crate) fn refresh_history(&self) {
        let Some(ui) = self.ui() else { return };
        let history = self.history.borrow();
        ui.set_can_undo(history.can_undo());
        ui.set_can_redo(history.can_redo());
    }

    pub(crate) fn undo(self: &Rc<Self>) {
        let current = self.app.borrow().config.clone();
        let restored = self.history.borrow_mut().undo(&current);
        match restored {
            Some((config, label)) => self.restore(config, &format!("已撤销：{label}")),
            None => self.toast("没有可以撤销的了"),
        }
    }

    pub(crate) fn redo(self: &Rc<Self>) {
        let current = self.app.borrow().config.clone();
        let restored = self.history.borrow_mut().redo(&current);
        match restored {
            Some((config, label)) => self.restore(config, &format!("已重做：{label}")),
            None => self.toast("没有可以重做的了"),
        }
    }

    /// 换成撤销、重做出来的配置：暂停按键、开机自启保留现在的（撤销不管它们）
    fn restore(self: &Rc<Self>, mut config: Config, message: &str) {
        {
            let current = &self.app.borrow().config.settings;
            config.settings.enabled = current.enabled;
            config.settings.autostart = current.autostart;
        }
        config.normalize();
        self.app.borrow_mut().config = config;
        self.refresh_all();
        self.refresh_history();
        self.schedule_save();
        self.toast(message);
    }

    // ───────── 方案 ─────────

    pub(crate) fn refresh_profiles(&self) {
        let Some(ui) = self.ui() else { return };
        let (names, index, name) = {
            let app = self.app.borrow();
            let config = &app.config;
            let names: Vec<String> = config.profiles.iter().map(|p| p.name.clone()).collect();
            let index = config.profiles.iter().position(|p| p.id == config.active_profile_id).unwrap_or(0);
            (names, index, config.active_profile_name())
        };
        ui.set_profile_names(strings(names.iter().map(String::as_str)));
        ui.set_profile_index(index as i32);
        ui.set_profile_name(name.into());
    }

    pub(crate) fn profile_selected(self: &Rc<Self>, index: i32) {
        let target = self.app.borrow().config.profiles.get(index.max(0) as usize).map(|p| (p.id.clone(), p.name.clone()));
        let Some((id, name)) = target else { return };
        if id == self.app.borrow().config.active_profile_id {
            return;
        }
        self.checkpoint(None, &format!("切到方案「{name}」"));
        let names = self.app.borrow_mut().config.switch_profile(&id);
        self.refresh_all();
        self.schedule_save();
        if let Some((_, to)) = names {
            self.toast(&format!("已切到方案「{to}」"));
        }
    }

    pub(crate) fn profile_new(self: &Rc<Self>) {
        let initial = self.app.borrow().config.unique_profile_name("新方案");
        self.open_name(Naming::NewProfile, "新增方案", "新方案里所有按键都先空着，布局还是共用的。要带上现在的动作就用「复制」。", &initial, "新增并切过去");
    }

    pub(crate) fn profile_copy(self: &Rc<Self>) {
        let initial = {
            let config = &self.app.borrow().config;
            config.unique_profile_name(&format!("{} 副本", config.active_profile_name()))
        };
        self.open_name(Naming::CopyProfile, "复制当前方案", "新方案里每个按键的动作和按住时都和当前方案一样。", &initial, "复制并切过去");
    }

    pub(crate) fn profile_rename(self: &Rc<Self>) {
        let initial = self.app.borrow().config.active_profile_name();
        self.open_name(Naming::RenameProfile, "重命名方案", "最多 40 个字。", &initial, "改名");
    }

    pub(crate) fn profile_delete(self: &Rc<Self>) {
        let (id, name, count) = {
            let config = &self.app.borrow().config;
            (config.active_profile_id.clone(), config.active_profile_name(), config.profiles.len())
        };
        if count <= 1 {
            self.toast("至少要留一个方案");
            return;
        }
        self.open_confirm(
            "删除方案",
            &format!("删除方案「{name}」？这个方案里所有按键的动作都会删掉，删完切到相邻的方案。可以撤销。"),
            "删除",
            true,
            Confirm::DeleteProfile(id),
        );
    }

    // ───────── 对话框 ─────────

    pub(crate) fn open_confirm(&self, title: &str, body: &str, ok: &str, danger: bool, action: Confirm) {
        let Some(ui) = self.ui() else { return };
        *self.confirm.borrow_mut() = Some(action);
        ui.set_confirm_title(title.into());
        ui.set_confirm_body(body.into());
        ui.set_confirm_ok(ok.into());
        ui.set_confirm_danger(danger);
        ui.set_confirm_notice(false);
        ui.set_confirm_open(true);
    }

    /// 只是告诉一声（报错）：一个「知道了」，Esc、点遮罩都能关
    pub(crate) fn notice(&self, title: &str, body: &str) {
        self.open_confirm(title, body, "知道了", false, Confirm::Nothing);
        if let Some(ui) = self.ui() {
            ui.set_confirm_notice(true);
        }
    }

    pub(crate) fn confirm_cancel(self: &Rc<Self>) {
        *self.confirm.borrow_mut() = None;
        if let Some(ui) = self.ui() {
            ui.set_confirm_open(false);
        }
        self.restore_focus();
    }

    /// 键盘焦点还给主窗口（获得焦点的东西被关掉、重建以后焦点就没了，撤销的快捷键也就没人收）
    pub(crate) fn restore_focus(&self) {
        if let Some(ui) = self.ui() {
            ui.invoke_restore_focus();
        }
    }

    pub(crate) fn confirm_accept(self: &Rc<Self>) {
        let action = self.confirm.borrow_mut().take();
        if let Some(ui) = self.ui() {
            ui.set_confirm_open(false);
        }
        self.restore_focus();
        match action {
            None | Some(Confirm::Nothing) => {}
            Some(Confirm::DeleteProfile(id)) => {
                let name = self.app.borrow().config.profiles.iter().find(|p| p.id == id).map(|p| p.name.clone()).unwrap_or_default();
                self.checkpoint(None, &format!("删除方案「{name}」"));
                let result = self.app.borrow_mut().config.delete_profile(&id);
                match result {
                    Ok(()) => {
                        self.refresh_all();
                        self.schedule_save();
                        let now = self.app.borrow().config.active_profile_name();
                        self.toast_undo(&format!("已删除方案「{name}」，现在是「{now}」"));
                    }
                    Err(e) => self.toast(&e),
                }
            }
            Some(Confirm::SharedSwitch { module, trigger, action }) => {
                self.checkpoint(None, "设置切换方案");
                self.app.borrow_mut().config.apply_shared_switch(&module, trigger, &action);
                self.actions_restructured();
                self.toast_undo("已应用到所有方案");
            }
            Some(Confirm::RemoveSwitch { module, trigger, steps }) => {
                self.checkpoint(None, "删除切换方案");
                self.app.borrow_mut().config.remove_shared_switch(&module, trigger);
                let steps: Vec<Step> = steps.into_iter().filter(|s| !is_switch(s)).collect();
                self.edit_steps(|current| *current = steps);
                self.actions_restructured();
                self.toast_undo("其他方案里这个位置的「切换方案」也一起删了");
            }
            Some(Confirm::ImportConfig(imported)) => self.replace_config(*imported),
            Some(Confirm::DeletePage(id)) => self.do_delete_page(&id),
            Some(Confirm::DeleteModule(page, module)) => self.do_delete_module(&page, &module),
            Some(Confirm::DefaultConfig) => {
                let mut fresh = Config::fresh();
                fresh.settings.autostart = self.app.borrow().config.settings.autostart;
                self.checkpoint(None, "换成默认布局");
                {
                    let mut app = self.app.borrow_mut();
                    app.config = fresh;
                    app.page = 0;
                    app.selected = None;
                    app.map_index = 0;
                    app.map_trigger = Trigger::Click;
                }
                *self.broken.borrow_mut() = None;
                self.changed();
                self.flush();
                self.toast_undo("已换成默认布局");
            }
        }
    }

    pub(crate) fn open_name(&self, purpose: Naming, title: &str, note: &str, initial: &str, ok: &str) {
        let Some(ui) = self.ui() else { return };
        *self.naming.borrow_mut() = Some(purpose);
        ui.set_name_title(title.into());
        ui.set_name_note(note.into());
        ui.set_name_initial(initial.into());
        ui.set_name_ok(ok.into());
        ui.set_name_open(true);
    }

    pub(crate) fn name_cancel(self: &Rc<Self>) {
        *self.naming.borrow_mut() = None;
        if let Some(ui) = self.ui() {
            ui.set_name_open(false);
        }
        self.restore_focus();
    }

    pub(crate) fn name_accept(self: &Rc<Self>, text: SharedString) {
        let Some(name) = config::clean_profile_name(&text) else {
            self.toast("名字不能是空的");
            return;
        };
        let purpose = self.naming.borrow_mut().take();
        if let Some(ui) = self.ui() {
            ui.set_name_open(false);
        }
        self.restore_focus();
        match purpose {
            None => {}
            Some(Naming::NewProfile) | Some(Naming::CopyProfile) => {
                let copy = matches!(purpose, Some(Naming::CopyProfile));
                self.checkpoint(None, &if copy { format!("复制成方案「{name}」") } else { format!("新增方案「{name}」") });
                let names = {
                    let mut app = self.app.borrow_mut();
                    let source = app.config.active_profile_id.clone();
                    let id = app.config.add_profile(&name, copy.then_some(source.as_str()));
                    app.config.switch_profile(&id)
                };
                self.refresh_all();
                self.schedule_save();
                if let Some((_, to)) = names {
                    self.toast(&if copy { format!("已复制成方案「{to}」并切过去") } else { format!("已新建方案「{to}」并切过去，所有按键都先空着") });
                }
            }
            Some(Naming::RenameProfile) => {
                self.checkpoint(None, &format!("把方案改名为「{name}」"));
                let result = {
                    let mut app = self.app.borrow_mut();
                    let id = app.config.active_profile_id.clone();
                    app.config.rename_profile(&id, &name)
                };
                match result {
                    Ok(name) => {
                        self.refresh_profiles();
                        self.refresh_overview();
                        self.schedule_save();
                        self.toast(&format!("已改名为「{name}」"));
                    }
                    Err(e) => self.toast(&e),
                }
            }
            Some(Naming::SaveSequence) => {
                let steps = self.current_steps();
                if steps.is_empty() {
                    self.toast("这里还没有动作，没东西可存");
                    return;
                }
                self.checkpoint(None, &format!("保存序列「{name}」"));
                {
                    let mut app = self.app.borrow_mut();
                    let id = app.config.new_sequence_id();
                    app.config.saved_sequences.push(SavedSequence { id, name: name.clone(), steps, ..Default::default() });
                }
                self.refresh_sequences();
                self.schedule_save();
                self.toast(&format!("已保存为「{name}」"));
            }
            Some(Naming::RenameSequence(index)) => {
                self.checkpoint(None, &format!("把序列改名为「{name}」"));
                if let Some(saved) = self.app.borrow_mut().config.saved_sequences.get_mut(index) {
                    saved.name = name;
                }
                self.refresh_sequences();
                self.schedule_save();
            }
        }
    }

    // ───────── 改整串动作 ─────────

    /// 映射页正在编辑的：（模块 id，按法，「按键 · 单击」）
    fn map_context(&self) -> Option<(String, Trigger, String)> {
        let app = self.app.borrow();
        let &(pi, mi) = button_refs(&app.config).get(app.map_index)?;
        let module = &app.config.layout.pages[pi].modules[mi];
        Some((module.id.clone(), app.map_trigger, format!("{} · {}", module.title(), app.map_trigger.name())))
    }

    fn current_steps(&self) -> Vec<Step> {
        let app = self.app.borrow();
        button_refs(&app.config)
            .get(app.map_index)
            .map(|&(pi, mi)| app.config.layout.pages[pi].modules[mi].behaviors.get(app.map_trigger).clone())
            .unwrap_or_default()
    }

    /// 结构性地改当前按法的整串动作（增删、换顺序、换类型、预设、粘贴、序列），可以撤销（label 是撤销提示里的说法）。
    /// 「切换方案」的规则：放进来时所有方案的这个位置都换成只有它（先确认），保证切过去还能切回来；
    /// 删掉时其他方案里也有的话一起删（先确认）。真的改了返回 true
    pub(crate) fn change_steps(self: &Rc<Self>, key: Option<String>, label: &str, change: impl FnOnce(&mut Vec<Step>)) -> bool {
        let Some((module, trigger, title)) = self.map_context() else { return false };
        let old = self.current_steps();
        let mut new = old.clone();
        change(&mut new);
        if new == old {
            return false;
        }
        let added = new.iter().find(|s| is_switch(s) && !old.iter().any(|o| o.id == s.id && o.action == s.action)).map(|s| s.action.clone());
        if let Some(action) = added {
            let body = {
                let config = &self.app.borrow().config;
                let lines: Vec<String> = config
                    .profile_sequences(&module, trigger)
                    .into_iter()
                    .map(|(name, active, summary)| format!("· {name}{}：{summary}", if active { "（当前）" } else { "" }))
                    .collect();
                let mut body = format!(
                    "所有方案里「{title}」现有的动作都会换成只有「{}」这一步，保证切过去还能切回来：\n\n{}",
                    action.summary(),
                    lines.join("\n")
                );
                if config.profiles.len() < 2 {
                    body.push_str("\n\n现在只有一个方案，切换不会有效果：先在上面新增一个方案。");
                }
                body
            };
            self.open_confirm("切换方案 · 覆盖确认", &body, "确认覆盖", false, Confirm::SharedSwitch { module, trigger, action });
            return false;
        }
        if old.iter().any(is_switch) && !new.iter().any(is_switch) {
            let others = self.app.borrow().config.profiles_with_switch(&module, trigger);
            if others.len() > 1 {
                let body = format!("删掉「{title}」的「切换方案」？这些方案里同一个位置的也会一起删掉：{}。", others.join("、"));
                self.open_confirm("删除切换方案", &body, "确认删除", true, Confirm::RemoveSwitch { module, trigger, steps: new });
                return false;
            }
        }
        self.checkpoint(key, label);
        self.edit_steps(|steps| *steps = new);
        self.actions_restructured();
        true
    }

    // ───────── 预设、复制粘贴、已保存的序列 ─────────

    pub(crate) fn apply_preset(self: &Rc<Self>, index: i32) {
        let Some(preset) = presets().into_iter().nth(index.max(0) as usize) else { return };
        let steps: Vec<Step> = preset.actions.into_iter().map(Step::new).collect();
        let label = format!("把「{}」设为预设「{}」", self.map_place(), preset.name);
        if self.change_steps(None, &label, |current| *current = steps) {
            self.toast_undo(&format!("已设为「{}」", preset.name));
        }
    }

    pub(crate) fn copy_actions(self: &Rc<Self>) {
        let steps = self.current_steps();
        if steps.is_empty() {
            self.toast("这里还没有动作");
            return;
        }
        let (text, skipped) = sequence::serialize(&steps);
        if win::set_clipboard_text(&text) {
            let note = if skipped > 0 { format!("，{skipped} 步没选按键或看不懂，没复制") } else { String::new() };
            self.toast(&format!("已复制 {} 步，可以粘贴到别的按法{note}", steps.len() - skipped));
        } else {
            self.toast("复制失败：剪贴板被别的程序占着，再试一次");
        }
    }

    pub(crate) fn paste_actions(self: &Rc<Self>) {
        let Some(text) = win::get_clipboard_text() else {
            self.toast("剪贴板里没有文字：先在别的按法上点「复制动作」");
            return;
        };
        match sequence::parse(&text) {
            Err(e) => self.toast(&format!("粘贴不了：{e}")),
            Ok(steps) => {
                let count = steps.len();
                let label = format!("给「{}」粘贴动作", self.map_place());
                if self.change_steps(None, &label, |current| *current = steps) {
                    self.toast_undo(&format!("已粘贴 {count} 步，替换了原来的"));
                }
            }
        }
    }

    pub(crate) fn save_sequence(self: &Rc<Self>) {
        if self.current_steps().is_empty() {
            self.toast("这里还没有动作，没东西可存");
            return;
        }
        let initial = self.map_context().map(|(_, _, title)| title).unwrap_or_default();
        self.open_name(Naming::SaveSequence, "存为动作序列", "起个名字，以后可以插入或替换到别的按法。", &initial, "保存");
    }

    pub(crate) fn open_sequences(self: &Rc<Self>) {
        self.refresh_sequences();
        if let Some(ui) = self.ui() {
            ui.set_sequences_open(true);
        }
    }

    pub(crate) fn refresh_sequences(&self) {
        let Some(ui) = self.ui() else { return };
        let items: Vec<SavedItem> = self
            .app
            .borrow()
            .config
            .saved_sequences
            .iter()
            .map(|s| SavedItem { name: s.name.as_str().into(), summary: config::summarize(&s.steps).into(), count: s.steps.len() as i32 })
            .collect();
        ui.set_saved_items(ModelRc::new(VecModel::from(items)));
        ui.set_sequences_target(self.map_context().map(|(_, _, title)| title).unwrap_or_default().into());
        ui.set_sequences_count(self.current_steps().len() as i32);
    }

    fn saved_steps(&self, index: i32) -> Option<(String, Vec<Step>)> {
        let app = self.app.borrow();
        let saved = app.config.saved_sequences.get(index.max(0) as usize)?;
        // 每次用都给新的 id，和已有的步骤不重复
        Some((saved.name.clone(), saved.steps.iter().map(|s| Step { id: actions::new_step_id(), ..s.clone() }).collect()))
    }

    pub(crate) fn sequence_insert(self: &Rc<Self>, index: i32) {
        let Some((name, steps)) = self.saved_steps(index) else { return };
        let label = format!("把序列「{name}」插到「{}」", self.map_place());
        if self.change_steps(None, &label, |current| current.extend(steps)) {
            self.refresh_sequences();
            self.toast_undo(&format!("已把「{name}」插到最后"));
        }
    }

    pub(crate) fn sequence_replace(self: &Rc<Self>, index: i32) {
        let Some((name, steps)) = self.saved_steps(index) else { return };
        let label = format!("把「{}」换成序列「{name}」", self.map_place());
        if self.change_steps(None, &label, |current| *current = steps) {
            self.refresh_sequences();
            self.toast_undo(&format!("已换成「{name}」"));
        }
    }

    pub(crate) fn sequence_rename(self: &Rc<Self>, index: i32) {
        let name = self.app.borrow().config.saved_sequences.get(index.max(0) as usize).map(|s| s.name.clone());
        if let Some(name) = name {
            self.open_name(Naming::RenameSequence(index.max(0) as usize), "给序列改名", "", &name, "改名");
        }
    }

    pub(crate) fn sequence_delete(self: &Rc<Self>, index: i32) {
        let index = index.max(0) as usize;
        let Some(name) = self.app.borrow().config.saved_sequences.get(index).map(|s| s.name.clone()) else { return };
        self.checkpoint(None, &format!("删除序列「{name}」"));
        self.app.borrow_mut().config.saved_sequences.remove(index);
        self.refresh_sequences();
        self.schedule_save();
        self.toast_undo(&format!("已删除序列「{name}」"));
    }

    pub(crate) fn sequences_close(self: &Rc<Self>) {
        if let Some(ui) = self.ui() {
            ui.set_sequences_open(false);
        }
        self.restore_focus();
    }

    // ───────── 录按键 ─────────

    fn set_record_hint(&self, held: &[&str]) {
        if let Some(ui) = self.ui() {
            ui.global::<Recorder>().set_hint(held.join(" + ").into());
        }
    }

    /// 录按键时按下、松开的键：返回 0 接着录、1 录好了（已经填进去）、2 取消
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_key(self: &Rc<Self>, index: i32, text: SharedString, ctrl: bool, alt: bool, shift: bool, meta: bool, pressed: bool) -> i32 {
        let Some(c) = text.chars().next() else { return 0 };
        let modifier = modifier_name(c);
        if pressed {
            if c == char::from(Key::Escape) {
                *self.record.borrow_mut() = Record::default();
                self.set_record_hint(&[]);
                self.restore_focus();
                return 2;
            }
            if let Some(name) = modifier {
                let held = {
                    let mut record = self.record.borrow_mut();
                    if !record.held.contains(&name) {
                        record.held.push(name);
                    }
                    record.held.clone()
                };
                self.set_record_hint(&held);
                return 0;
            }
            let Some(key) = key_name(c) else {
                self.toast("这个键录不了，换一个");
                return 0;
            };
            let held = std::mem::take(&mut self.record.borrow_mut().held);
            // 录之前就按着的修饰键看不到按下，就按标志补上
            let mut keys: Vec<String> = held.iter().map(|m| m.to_string()).collect();
            for (on, name) in [(ctrl, "Ctrl"), (alt, "Alt"), (shift, "Shift"), (meta, "Win")] {
                let family = modifier_rank(name);
                if on && !keys.iter().any(|k| modifier_rank(k) == family) {
                    keys.push(name.to_string());
                }
            }
            keys.sort_by_key(|k| modifier_rank(k));
            keys.push(key);
            self.set_record_hint(&[]);
            self.set_recorded(index, keys);
            return 1;
        }
        // 松开：只按了修饰键就松开，录这些修饰键（比如单独一个 Ctrl）
        if modifier.is_some() {
            let held = std::mem::take(&mut self.record.borrow_mut().held);
            if !held.is_empty() {
                let mut keys: Vec<String> = held.iter().map(|m| m.to_string()).collect();
                keys.sort_by_key(|k| modifier_rank(k));
                self.set_recorded(index, keys);
                self.set_record_hint(&[]);
                return 1;
            }
        }
        0
    }

    /// 录到的键填进第 index 步：「按键」录到一个键就是这个键，录到组合键就改成「组合键」
    fn set_recorded(self: &Rc<Self>, index: i32, keys: Vec<String>) {
        let text = keys.join("+");
        self.checkpoint(None, &format!("给「{}」第 {} 步录了 {text}", self.map_place(), index + 1));
        let mut became_combo = false;
        self.edit_step(index, |step| match &mut step.action {
            Action::Shortcut { keys: current } => *current = keys,
            Action::Key { key } if keys.len() == 1 => *key = keys[0].clone(),
            Action::Key { .. } => {
                step.action = Action::Shortcut { keys };
                became_combo = true;
            }
            _ => {}
        });
        self.actions_restructured();
        self.restore_focus();
        self.toast(&if became_combo { format!("录到的是组合键 {text}，这一步改成了「组合键」") } else { format!("已录入 {text}") });
    }

    // ───────── 设置：导出导入备份、日志 ─────────

    /// 导出备份：布局、所有方案、设置和已保存的序列，存成一个文件（文件对话框在别的线程里开）
    pub(crate) fn export_backup(self: &Rc<Self>) {
        let name = format!("蒜鸟备份-{}.json", win::file_stamp());
        let text = transfer::export_document(&self.app.borrow().config, true, env!("CARGO_PKG_VERSION"));
        win::file_dialog(win::FileDialog::SaveJson(name), move |path| {
            with_ctx(|ctx| {
                if let Some(path) = path {
                    ctx.export_done(path, text);
                }
            })
        });
    }

    fn export_done(self: &Rc<Self>, path: String, text: String) {
        match std::fs::write(&path, text) {
            Ok(()) => {
                let file = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
                self.show_toast(&format!("已导出 {file}"), Some(ToastAction::Reveal(path)), "打开所在文件夹");
            }
            Err(e) => self.notice("导出失败", &format!("写不了这个文件：{e}")),
        }
    }

    pub(crate) fn import_config(self: &Rc<Self>) {
        win::file_dialog(win::FileDialog::Backup, |path| {
            with_ctx(|ctx| {
                if let Some(path) = path {
                    ctx.backup_picked(path);
                }
            })
        });
    }

    fn backup_picked(self: &Rc<Self>, path: String) {
        let text = match std::fs::read(&path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => return self.notice("读不了这个文件", &e.to_string()),
        };
        let file = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(path);
        match transfer::parse_import(&text) {
            Err(e) => self.notice("导入不了这个文件", &format!("{file}：{e}\n\n现在的配置什么都没改。")),
            Ok((imported, summary)) => {
                let mut lines = vec![format!(
                    "· 文件：{file}（{}{}）",
                    if summary.backup { "备份" } else { "旧版导出的配置" },
                    match transfer::parse_iso_utc(&summary.exported_at) {
                        Some(secs) => format!("，导出于 {}", win::local_time_text(secs)),
                        None => String::new(),
                    }
                )];
                lines.push(format!("· 方案 {} 个：{}（当前：{}）", summary.profiles.len(), summary.profiles.join("、"), summary.active));
                lines.push(format!("· 布局 {} 页、{} 个模块，其中 {} 个按键", summary.pages, summary.modules, summary.buttons));
                lines.push(format!("· 已保存的动作序列 {} 个", summary.sequences));
                let body = format!(
                    "用这个文件替换现在的配置（布局、所有方案、设置；开机自启保持现在的）：\n\n{}\n\n替换前会先把现在的配置备份到 backups 文件夹，导入后也可以撤销。",
                    lines.join("\n")
                );
                self.open_confirm("导入备份", &body, "导入并替换", true, Confirm::ImportConfig(Box::new(imported)));
            }
        }
    }

    /// 导入：先备份现在的配置文件，再换成导入的（开机自启保持现在的），可以撤销
    fn replace_config(self: &Rc<Self>, mut imported: Config) {
        let dir = config::backups_dir();
        let backup = dir.join(format!("config-before-import-{}.json", win::file_stamp()));
        let saved = std::fs::create_dir_all(&dir).and_then(|_| {
            if config::config_path().exists() {
                std::fs::copy(config::config_path(), &backup).map(|_| ())
            } else {
                std::fs::write(&backup, config::to_text(&self.app.borrow().config))
            }
        });
        if let Err(e) = saved {
            self.notice("没有导入", &format!("备份现在的配置失败：{e}"));
            return;
        }
        self.checkpoint(None, "导入备份");
        {
            let mut app = self.app.borrow_mut();
            imported.settings.autostart = app.config.settings.autostart;
            app.config = imported;
            app.page = 0;
            app.selected = None;
            app.map_index = 0;
            app.map_trigger = Trigger::Click;
        }
        self.refresh_all();
        self.schedule_save();
        let name = backup.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.toast_undo(&format!("已导入。原来的配置备份在 backups\\{name}"));
    }

    pub(crate) fn open_logs(self: &Rc<Self>) {
        let dir = config::data_dir().join("logs");
        let _ = std::fs::create_dir_all(&dir);
        win::open_path(&dir);
    }
}
