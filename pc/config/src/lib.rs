//! 蒜鸟电脑端后台（SuanNIAO-core.exe）和主界面（SuanNIAO.exe）共用的配置。
//! 0.7.4 以前内部名字叫 PhonePad（PhonePad.exe、phonepad-core.exe、`%APPDATA%\PhonePad`），新版启动时把旧的换掉（见 move_legacy_data_dir）。
//!
//! 手机界面是一张网格（默认 4 列 × 8 行），上面摆若干模块：触控板、按键、语音输入框。
//! 模块的位置和大小以「格」为单位，可以是小数（见 geometry）。
//! 按键的单击 / 双击 / 长按各自对应一串动作，格式和 axonkey 的映射一致，方便导入（见 actions、axonkey）；
//! 按住时可以触发一次长按、重复长按、连续单击（连发），或者像实体键盘一样一直按着（直通），见 HoldMode。
//! 配置存在 `%APPDATA%\SuanNIAO\config.json`；主界面改完写文件，后台发现文件变了就重新读取并同步给手机。
//! 在手机上调好的布局由后台写回这个文件，主界面发现文件被改了会重新载入。

pub mod actions;
pub mod axonkey;
pub mod geometry;
pub mod leveldb;
pub mod merge;
pub mod presets;
pub mod profiles;
pub mod sequence;
pub mod transfer;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Map, Value};

pub use actions::{
    any_active, key_code, summarize, Action, PasteMode, Step, ACTION_KINDS, DIRECTIONS, KEY_GROUPS, MEDIA_KEYS, MOUSE_BUTTONS,
    PASTE_MODES,
};
pub use geometry::{Rect, MIN_SIZE, SNAP};
pub use profiles::{clean_profile_name, ButtonMapping, Profile, SavedSequence, PROFILE_DIRECTIONS};

/// 配置文件格式的版本：2 = 每步动作带 id、enabled，按键有「按住时」，有总开关；3 = 多套方案、已保存的动作序列；
/// 4 = 默认的方案改叫「默认方案」（读到 3 及以前的「默认配置」时改名）
pub const CONFIG_VERSION: u32 = 4;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub version: u32,
    pub layout: Layout,
    /// 多套方案（布局共用，每套只存按键的映射）；当前方案的映射同时写在模块上，见 profiles
    pub profiles: Vec<Profile>,
    pub active_profile_id: String,
    /// 起了名字保存下来的动作序列
    pub saved_sequences: Vec<SavedSequence>,
    pub settings: Settings,
    /// 更新的版本加的字段（比如以后的多套方案）：看不懂也原样留着，保存时写回
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Layout {
    pub columns: u32,
    pub rows: u32,
    pub pages: Vec<Page>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Page {
    pub id: String,
    pub name: String,
    pub modules: Vec<Module>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModuleKind {
    #[default]
    Button,
    Touchpad,
    Voice,
    VoiceButton,
}

impl ModuleKind {
    pub const ALL: [ModuleKind; 4] = [ModuleKind::Button, ModuleKind::Touchpad, ModuleKind::Voice, ModuleKind::VoiceButton];

    pub fn name(self) -> &'static str {
        match self {
            ModuleKind::Button => "按键",
            ModuleKind::Touchpad => "触控板",
            ModuleKind::Voice => "语音输入框",
            ModuleKind::VoiceButton => "语音按钮",
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            ModuleKind::Button => "button",
            ModuleKind::Touchpad => "touchpad",
            ModuleKind::Voice => "voice",
            ModuleKind::VoiceButton => "voiceButton",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Module {
    pub id: String,
    pub kind: ModuleKind,
    pub label: String,
    /// 位置和大小，单位是格，可以是小数；整数写进文件时不带小数点
    #[serde(serialize_with = "cells")]
    pub x: f64,
    #[serde(serialize_with = "cells")]
    pub y: f64,
    #[serde(serialize_with = "cells")]
    pub w: f64,
    #[serde(serialize_with = "cells")]
    pub h: f64,
    /// 只有按键用：按住时怎么办（见 HoldMode）。没选过是 None：单击只有一个滚轮或光标移动时连发，其余触发一次长按
    #[serde(deserialize_with = "lenient_hold", skip_serializing_if = "Option::is_none")]
    pub hold: Option<HoldMode>,
    /// 旧版（电脑 0.3.0、手机 0.5.0）的「按住连发」：读到 true 当作 hold = repeatClick；
    /// 保存时跟着 hold 写，旧版读到也还是连发
    pub repeat: bool,
    /// 只有按键用：单击 / 双击 / 长按各自的动作。
    pub behaviors: Behaviors,
    /// 更新的版本加的字段：原样留着
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// 按住按键时怎么办
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HoldMode {
    /// 按住到长按时间，触发一次长按（默认）
    LongPress,
    /// 到长按时间触发一次长按，隔「长按重复延迟」再一次，之后每「长按重复间隔」一次，松手停（axonkey 的规则 C）
    RepeatLongPress,
    /// 按下立刻单击一次，按住超过连发延迟后按连发间隔一直单击，松手停（连发）
    RepeatClick,
    /// 按下就按住单击里的键，松手才抬起，像实体键盘一样（axonkey 的规则 A）。只有单击正好是一个按键或组合键时能用
    Passthrough,
}

impl HoldMode {
    pub const ALL: [HoldMode; 4] = [HoldMode::LongPress, HoldMode::RepeatLongPress, HoldMode::RepeatClick, HoldMode::Passthrough];

    pub fn code(self) -> &'static str {
        match self {
            HoldMode::LongPress => "longPress",
            HoldMode::RepeatLongPress => "repeatLongPress",
            HoldMode::RepeatClick => "repeatClick",
            HoldMode::Passthrough => "passthrough",
        }
    }

    /// 四处（映射页、布局页、列表、表格）都用这个名字
    pub fn name(self) -> &'static str {
        match self {
            HoldMode::LongPress => "长按一次",
            HoldMode::RepeatLongPress => "重复长按",
            HoldMode::RepeatClick => "连发",
            HoldMode::Passthrough => "直通",
        }
    }

    /// 名字下面的一句说明
    pub fn note(self) -> &'static str {
        match self {
            HoldMode::LongPress => "按住到长按时间，执行一次「长按」",
            HoldMode::RepeatLongPress => "按住后反复执行「长按」，松手就停",
            HoldMode::RepeatClick => "按下就执行「单击」，按住一直重复，松手就停",
            HoldMode::Passthrough => "像实体键盘：按下就按住「单击」的键，松手才抬起",
        }
    }

    /// 双击、长按还判不判定（连发和直通时不判定）
    pub fn judges_other_triggers(self) -> bool {
        matches!(self, HoldMode::LongPress | HoldMode::RepeatLongPress)
    }
}

/// 读「按住时」：看不懂的值（更新的版本写的）当作没选过，不连累整份配置
pub(crate) fn lenient_hold<'de, D: Deserializer<'de>>(d: D) -> Result<Option<HoldMode>, D::Error> {
    Ok(Value::deserialize(d).ok().and_then(|v| serde_json::from_value(v).ok()))
}

/// 格数写进 JSON：整数不带小数点（和旧版的配置一样），小数照常写
fn cells<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    if v.fract() == 0.0 && v.abs() < 1e6 {
        s.serialize_i64(*v as i64)
    } else {
        s.serialize_f64(*v)
    }
}

/// 发给手机的格数，规则同上
pub fn num(v: f64) -> Value {
    if v.fract() == 0.0 && v.abs() < 1e6 { json!(v as i64) } else { json!(v) }
}

impl Module {
    /// 手机上显示的文字：按键用自己的名字，其他模块用类型名。
    pub fn title(&self) -> String {
        if self.kind == ModuleKind::Button && !self.label.is_empty() { self.label.clone() } else { self.kind.name().to_string() }
    }

    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.w, self.h)
    }

    pub fn set_rect(&mut self, rect: Rect) {
        let rect = rect.tidy();
        (self.x, self.y, self.w, self.h) = (rect.x, rect.y, rect.w, rect.h);
    }

    /// 按住时实际怎么办。直通要求单击正好是一个启用的按键或组合键，不满足就退回「触发一次长按」
    pub fn hold_mode(&self) -> HoldMode {
        if self.kind != ModuleKind::Button {
            return HoldMode::LongPress;
        }
        let wanted = self.hold.unwrap_or(if self.repeat { HoldMode::RepeatClick } else { self.default_hold() });
        if wanted == HoldMode::Passthrough && self.passthrough_keys().is_none() {
            HoldMode::LongPress
        } else {
            wanted
        }
    }

    /// 没选过时的默认：单击只有一个滚轮或光标移动、又没配双击和长按，就连发（和 axonkey 一样）
    pub fn default_hold(&self) -> HoldMode {
        let b = &self.behaviors;
        let mut active = b.click.iter().filter(|s| s.active());
        let single = match (active.next(), active.next()) {
            (Some(step), None) => matches!(step.action, Action::Wheel { .. } | Action::CursorMove { .. }),
            _ => false,
        };
        if single && !any_active(&b.double_click) && !any_active(&b.long_press) {
            HoldMode::RepeatClick
        } else {
            HoldMode::LongPress
        }
    }

    /// 直通时要按住的键：单击正好是一个启用的按键或组合键
    pub fn passthrough_keys(&self) -> Option<Vec<String>> {
        let mut active = self.behaviors.click.iter().filter(|s| s.active());
        match (active.next(), active.next()) {
            (Some(step), None) => step.action.chord(),
            _ => None,
        }
    }

    pub fn set_hold(&mut self, mode: HoldMode) {
        self.hold = Some(mode);
        self.repeat = mode == HoldMode::RepeatClick;
    }

    /// 按住连发（界面、统计用）
    pub fn repeats(&self) -> bool {
        self.hold_mode() == HoldMode::RepeatClick
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Click,
    DoubleClick,
    LongPress,
}

impl Trigger {
    pub const ALL: [Trigger; 3] = [Trigger::Click, Trigger::DoubleClick, Trigger::LongPress];

    pub fn code(self) -> &'static str {
        match self {
            Self::Click => "click",
            Self::DoubleClick => "doubleClick",
            Self::LongPress => "longPress",
        }
    }

    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "click" => Some(Self::Click),
            "doubleClick" => Some(Self::DoubleClick),
            "longPress" => Some(Self::LongPress),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Click => "单击",
            Self::DoubleClick => "双击",
            Self::LongPress => "长按",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Behaviors {
    pub click: Vec<Step>,
    pub double_click: Vec<Step>,
    pub long_press: Vec<Step>,
    /// 更新的版本加的按法：原样留着
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Behaviors {
    pub fn get(&self, trigger: Trigger) -> &Vec<Step> {
        match trigger {
            Trigger::Click => &self.click,
            Trigger::DoubleClick => &self.double_click,
            Trigger::LongPress => &self.long_press,
        }
    }

    pub fn get_mut(&mut self, trigger: Trigger) -> &mut Vec<Step> {
        match trigger {
            Trigger::Click => &mut self.click,
            Trigger::DoubleClick => &mut self.double_click,
            Trigger::LongPress => &mut self.long_press,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// 总开关：false = 暂停按键映射（按键不执行动作，触控板和语音照常）
    pub enabled: bool,
    pub touchpad: TouchpadSettings,
    pub voice: VoiceSettings,
    pub triggers: TriggerSettings,
    pub autostart: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TouchpadSettings {
    pub sensitivity: f64,
    pub acceleration: f64,
    pub scroll_speed: f64,
    pub natural_scroll: bool,
    pub smoothing: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VoiceSettings {
    pub auto_send: bool,
    pub delay_ms: u32,
    pub enter_after: bool,
    /// true：剪贴板粘贴；false：逐字输入
    pub paste: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TriggerSettings {
    pub double_click_ms: u32,
    pub long_press_ms: u32,
    /// 按住连发：按住多久开始重复
    pub repeat_delay_ms: u32,
    /// 按住连发：重复时每两次之间隔多久
    pub repeat_interval_ms: u32,
    /// 重复长按：第一次长按之后隔多久再来一次
    pub long_press_repeat_delay_ms: u32,
    /// 重复长按：之后每两次之间隔多久
    pub long_press_repeat_interval_ms: u32,
    /// 连发「光标移动」时的加速度 0～100：按住 3 秒内线性加速到 (1 + 加速度/100) 倍
    pub cursor_acceleration: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            layout: Layout::default(),
            profiles: Vec::new(),
            active_profile_id: String::new(),
            saved_sequences: Vec::new(),
            settings: Settings::default(),
            extra: Map::new(),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            touchpad: TouchpadSettings::default(),
            voice: VoiceSettings::default(),
            triggers: TriggerSettings::default(),
            autostart: false,
            extra: Map::new(),
        }
    }
}

impl Default for TouchpadSettings {
    fn default() -> Self {
        Self { sensitivity: 2.5, acceleration: 1.2, scroll_speed: 1.0, natural_scroll: true, smoothing: true }
    }
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self { auto_send: true, delay_ms: 1000, enter_after: false, paste: true }
    }
}

impl Default for TriggerSettings {
    fn default() -> Self {
        // 新配置的双击、长按时间按手机触屏定（300 / 500）；旧配置里写了的照旧
        Self {
            double_click_ms: 300,
            long_press_ms: 500,
            repeat_delay_ms: 400,
            repeat_interval_ms: 50,
            long_press_repeat_delay_ms: 350,
            long_press_repeat_interval_ms: 100,
            cursor_acceleration: 50,
        }
    }
}

impl Default for Layout {
    /// 默认布局：上半屏语音按钮 + 常用键，左下 1/4 触控板，右下回车、换行、删除。
    /// 方向键、换行、删除默认按住连发（连发时长按不判定，所以删除不配长按）。
    fn default() -> Self {
        let step = |id: String, action: Action| Step { id, enabled: true, action };
        let button = |id: &str, label: &str, (x, y, w, h): (f64, f64, f64, f64), click: Action, long_press: Option<Action>, repeat: bool| Module {
            id: id.into(),
            kind: ModuleKind::Button,
            label: label.into(),
            x,
            y,
            w,
            h,
            hold: repeat.then_some(HoldMode::RepeatClick),
            repeat,
            behaviors: Behaviors {
                click: vec![step(format!("{id}-click-1"), click)],
                double_click: vec![],
                long_press: long_press.into_iter().map(|a| step(format!("{id}-longPress-1"), a)).collect(),
                extra: Map::new(),
            },
            extra: Map::new(),
        };
        let key = |k: &str| Action::Key { key: k.into() };
        let combo = |ks: &[&str]| Action::Shortcut { keys: ks.iter().map(|s| s.to_string()).collect() };
        let area = |id: &str, kind: ModuleKind, (x, y, w, h): (f64, f64, f64, f64)| Module {
            id: id.into(),
            kind,
            label: kind.name().into(),
            x,
            y,
            w,
            h,
            hold: None,
            repeat: false,
            behaviors: Behaviors::default(),
            extra: Map::new(),
        };
        Layout {
            columns: 4,
            rows: 8,
            pages: vec![Page {
                id: "main".into(),
                name: "主页".into(),
                modules: vec![
                    area("voice-button", ModuleKind::VoiceButton, (0.0, 0.0, 4.0, 2.0)),
                    button("esc", "Esc", (0.0, 2.0, 1.0, 1.0), key("Esc"), None, false),
                    button("shift-tab", "Shift+Tab", (1.0, 2.0, 1.0, 1.0), combo(&["Shift", "Tab"]), None, false),
                    button("up", "↑", (2.0, 2.0, 1.0, 1.0), key("Up"), None, true),
                    button("down", "↓", (3.0, 2.0, 1.0, 1.0), key("Down"), None, true),
                    button("ctrl-c", "^C", (0.0, 3.0, 1.0, 1.0), combo(&["Ctrl", "C"]), None, false),
                    button("tab", "Tab", (1.0, 3.0, 1.0, 1.0), key("Tab"), None, false),
                    button("left", "←", (2.0, 3.0, 1.0, 1.0), key("Left"), None, true),
                    button("right", "→", (3.0, 3.0, 1.0, 1.0), key("Right"), None, true),
                    area("touchpad", ModuleKind::Touchpad, (0.0, 4.0, 2.0, 4.0)),
                    button("enter", "回车", (2.0, 4.0, 2.0, 2.0), key("Enter"), None, false),
                    button("newline", "换行", (2.0, 6.0, 1.0, 2.0), combo(&["Shift", "Enter"]), None, true),
                    button("backspace", "删除", (3.0, 6.0, 1.0, 2.0), key("Backspace"), None, true),
                ],
            }],
        }
    }
}

impl Config {
    /// 全新的配置（第一次用、或者选了「用默认布局」）：默认布局，规整好（有「默认方案」）
    pub fn fresh() -> Self {
        let mut config = Config::default();
        config.normalize();
        config
    }

    /// 读进来以后规整一下：没有 id 的步骤按位置补上（和 axonkey 一样是「键-按法-序号」，电脑上两个程序读同一份文件补出来的一样），
    /// 旧的 repeat 换成 hold，repeat 跟着 hold 写（旧版也读得懂连发）；方案见 profiles（没有就把现在的映射做成「默认配置」）；
    /// 已保存的序列补 id 和名字
    pub fn normalize(&mut self) {
        // 3 及以前的文件：默认的方案叫「默认配置」，改成「默认方案」
        let rename_default = self.version < 4;
        self.version = self.version.max(CONFIG_VERSION);
        for module in self.layout.pages.iter_mut().flat_map(|p| p.modules.iter_mut()) {
            if module.hold.is_none() && module.repeat {
                module.hold = Some(HoldMode::RepeatClick);
            }
            module.repeat = module.kind == ModuleKind::Button && module.hold == Some(HoldMode::RepeatClick);
            let id = module.id.clone();
            for trigger in Trigger::ALL {
                number_steps(&id, trigger, module.behaviors.get_mut(trigger));
            }
        }
        self.normalize_profiles(rename_default);
        let mut ids = std::collections::HashSet::new();
        for (i, saved) in self.saved_sequences.iter_mut().enumerate() {
            if saved.id.trim().is_empty() || ids.contains(&saved.id) {
                saved.id = format!("sequence-{}", i + 1);
                while ids.contains(&saved.id) {
                    saved.id.push('x');
                }
            }
            ids.insert(saved.id.clone());
            saved.name = clean_profile_name(&saved.name).unwrap_or_else(|| format!("序列 {}", i + 1));
            let sid = saved.id.clone();
            number_steps(&sid, Trigger::Click, &mut saved.steps);
        }
    }

    /// 按 id 找按键模块（所有页面里找）。
    pub fn find_module(&self, id: &str) -> Option<&Module> {
        self.layout.pages.iter().flat_map(|p| p.modules.iter()).find(|m| m.id == id)
    }

    /// 网格大小（列, 行），算几何用
    pub fn grid(&self) -> (f64, f64) {
        (self.layout.columns as f64, self.layout.rows as f64)
    }

    /// 第 page 页除了 skip 以外所有模块的位置
    pub fn others(&self, page: usize, skip: Option<usize>) -> Vec<Rect> {
        let Some(page) = self.layout.pages.get(page) else { return Vec::new() };
        page.modules.iter().enumerate().filter(|(i, _)| Some(*i) != skip).map(|(_, m)| m.rect()).collect()
    }

    /// 某个矩形能不能放进第 page 页：不出界、不小于最小尺寸、不和别的模块重叠（skip 是正在移动的模块自己）。
    pub fn fits(&self, page: usize, skip: Option<usize>, rect: Rect) -> bool {
        let (columns, rows) = self.grid();
        page < self.layout.pages.len() && rect.inside(columns, rows) && geometry::is_free(&rect, &self.others(page, skip))
    }

    /// 在第 page 页找一块能放下 w×h 的空位（按 1/4 格从上到下、从左到右找）。
    pub fn free_spot(&self, page: usize, w: f64, h: f64) -> Option<(f64, f64)> {
        let (columns, rows) = self.grid();
        let steps = |total: f64, size: f64| ((total - size) / SNAP + 1e-6).floor() as i64;
        (0..=steps(rows, h))
            .flat_map(|yi| (0..=steps(columns, w)).map(move |xi| (xi as f64 * SNAP, yi as f64 * SNAP)))
            .find(|&(x, y)| self.fits(page, None, Rect::new(x, y, w, h)))
    }

    /// 检查整个布局：每个模块都在网格里、不小于最小尺寸、互不重叠。有问题返回第一个问题的说明。
    pub fn check_layout(&self) -> Result<(), String> {
        let (columns, rows) = self.grid();
        self.layout.pages.iter().try_for_each(|page| check_modules(&page.modules, columns, rows))
    }

    /// 手机上调好的布局：把 page_id 这一页里这些模块挪到新位置、改成新大小，返回改了几个。
    /// 不认识的 id（比如刚在电脑上删掉的模块）跳过；改完整页检查一遍，出界、太小或重叠就整个不改。
    pub fn set_page_layout(&mut self, page_id: &str, items: &[(String, Rect)]) -> Result<usize, String> {
        let (columns, rows) = self.grid();
        let Some(page) = self.layout.pages.iter_mut().find(|p| p.id == page_id) else {
            return Err("电脑上已经没有这一页了".into());
        };
        let mut modules = page.modules.clone();
        let mut applied = 0;
        for (id, rect) in items {
            if ![rect.x, rect.y, rect.w, rect.h].iter().all(|v| v.is_finite()) {
                return Err("位置数据不对".into());
            }
            if let Some(m) = modules.iter_mut().find(|m| &m.id == id) {
                m.set_rect(*rect);
                applied += 1;
            }
        }
        check_modules(&modules, columns, rows)?;
        page.modules = modules;
        Ok(applied)
    }

    /// 两份配置是不是只有模块的位置、大小不一样（用来提示「手机上调了布局」）
    pub fn same_except_geometry(&self, other: &Config) -> bool {
        let strip = |c: &Config| {
            let mut c = c.clone();
            c.layout.pages.iter_mut().flat_map(|p| p.modules.iter_mut()).for_each(|m| m.set_rect(Rect::default()));
            serde_json::to_value(c).ok()
        };
        strip(self) == strip(other)
    }

    /// 生成不重复的模块 id。
    pub fn new_id(&self, prefix: &str) -> String {
        let used: Vec<&str> = self.layout.pages.iter().flat_map(|p| p.modules.iter()).map(|m| m.id.as_str()).chain(self.layout.pages.iter().map(|p| p.id.as_str())).collect();
        (1..).map(|n| format!("{prefix}-{n}")).find(|id| !used.contains(&id.as_str())).unwrap()
    }

    /// 发给手机的部分：布局、各按键有哪些按法和按住时怎么办、触控板和语音设置、是不是暂停了。动作本身留在电脑上执行。
    /// 「有没有这种按法」看有没有启用的步骤（只剩禁用或停用的步骤算没有）；连发、直通的按键双击、长按一律报成没有。
    pub fn phone_payload(&self) -> Value {
        let pages: Vec<Value> = self
            .layout
            .pages
            .iter()
            .map(|page| {
                let modules: Vec<Value> = page
                    .modules
                    .iter()
                    .map(|m| {
                        let hold = m.hold_mode();
                        let others = hold.judges_other_triggers();
                        json!({
                            "id": m.id, "kind": m.kind.code(), "label": m.title(),
                            "x": num(m.x), "y": num(m.y), "w": num(m.w), "h": num(m.h),
                            "hold": hold.code(),
                            // 旧版 App（0.5.0）只认这个
                            "repeat": hold == HoldMode::RepeatClick,
                            "triggers": {
                                "click": any_active(&m.behaviors.click),
                                "doubleClick": others && any_active(&m.behaviors.double_click),
                                "longPress": others && any_active(&m.behaviors.long_press),
                            }
                        })
                    })
                    .collect();
                json!({"id": page.id, "name": page.name, "modules": modules})
            })
            .collect();
        let s = &self.settings;
        json!({
            "t": "config",
            "paused": !s.enabled,
            // 当前方案（手机上显示用）
            "profile": {"id": self.active_profile_id, "name": self.active_profile_name(), "count": self.profiles.len()},
            "layout": {
                "columns": self.layout.columns, "rows": self.layout.rows,
                "snap": SNAP, "minSize": MIN_SIZE,
                "pages": pages,
            },
            "touchpad": {
                "sensitivity": s.touchpad.sensitivity, "acceleration": s.touchpad.acceleration,
                "scrollSpeed": s.touchpad.scroll_speed, "naturalScroll": s.touchpad.natural_scroll,
            },
            "voice": {"autoSend": s.voice.auto_send, "delayMs": s.voice.delay_ms},
            "triggers": {
                "doubleClickMs": s.triggers.double_click_ms, "longPressMs": s.triggers.long_press_ms,
                "repeatDelayMs": s.triggers.repeat_delay_ms, "repeatIntervalMs": s.triggers.repeat_interval_ms,
                "longPressRepeatDelayMs": s.triggers.long_press_repeat_delay_ms,
                "longPressRepeatIntervalMs": s.triggers.long_press_repeat_interval_ms,
            },
        })
    }
}

/// 没有 id（或者重复）的步骤按位置补上：「键-按法-序号」
pub(crate) fn number_steps(owner: &str, trigger: Trigger, steps: &mut [Step]) {
    let code = trigger.code();
    let mut seen = std::collections::HashSet::new();
    for (i, step) in steps.iter_mut().enumerate() {
        if step.id.is_empty() || seen.contains(&step.id) {
            let mut candidate = format!("{owner}-{code}-{}", i + 1);
            while seen.contains(&candidate) {
                candidate.push('x');
            }
            step.id = candidate;
        }
        seen.insert(step.id.clone());
    }
}

/// 一页里的模块：都在网格里、不小于最小尺寸、互不重叠
fn check_modules(modules: &[Module], columns: f64, rows: f64) -> Result<(), String> {
    for (i, m) in modules.iter().enumerate() {
        let rect = m.rect();
        if !rect.inside(columns, rows) {
            return Err(format!("「{}」超出了屏幕，或者小于 {} 格", m.title(), geometry::show(MIN_SIZE)));
        }
        if let Some(o) = modules[i + 1..].iter().find(|o| o.rect().overlaps(&rect)) {
            return Err(format!("「{}」和「{}」重叠了", m.title(), o.title()));
        }
    }
    Ok(())
}

/// 后台写、主界面读的运行状态。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Status {
    pub pid: u32,
    /// 后台的版本：主界面发现后台比自己旧（或者没写，是 0.3.0 以前的）就把它换成新的
    pub version: String,
    pub connected: bool,
    pub device: String,
    /// 连上的时刻（Unix 秒）
    pub since: u64,
    /// 状态文件最后更新时刻（Unix 秒），后台每 5 秒刷新一次，用来判断后台还活着
    pub updated: u64,
    pub memory_mb: f64,
    /// 下面是「今天」的用量。day 是本地日期（2026-10-29），换了一天就清零
    pub day: String,
    /// 今天按了几次键（手机上的按键模块）
    pub presses: u32,
    /// 今天语音输入了多少字
    pub voice_chars: u32,
    /// 每个按键（模块 id）今天按了几次
    pub per_button: BTreeMap<String, u32>,
    /// 最近做了什么，新的在前，最多 RECENT_MAX 条
    pub recent: Vec<Recent>,
    /// 手机电量（连上以后手机报上来；没报过、断开了是 None）
    pub battery: Option<Battery>,
    /// 手机的型号（Build.MODEL）和 App 版本：hello 里报上来，断开后留着（关于页、手机信息浮层用）
    pub phone_model: String,
    pub phone_app: String,
    /// 配置推送的回执：后台每推一次配置 config_rev 加一（推送里带 rev），手机套用完回 configAck，
    /// 记下 ack_rev 和时刻 ack_at；config_at 是最后一次推送的时刻（Unix 秒）
    pub config_rev: u64,
    pub config_at: u64,
    pub ack_rev: u64,
    pub ack_at: u64,
    /// 配置文件读不出来（后台在用上一份好的配置或者最近的备份）；读好了就是 None
    pub config_error: Option<ConfigError>,
    /// 蓝牙串口服务没开起来的原因（电脑蓝牙关着之类）；开起来了是空的
    pub bluetooth_error: String,
}

/// 配置文件读不出来：原因、坏文件另存成了什么、现在用的是什么
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ConfigError {
    pub message: String,
    /// 另存的坏文件名（config.broken-时间.json）
    pub broken: String,
    /// 现在用的：「上一份好的配置」「备份 xxx」「默认布局」
    pub using: String,
    pub at: u64,
}

/// 手机电量
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Battery {
    /// 0～100
    pub level: u8,
    pub charging: bool,
}

/// 「最近动作」里的一条
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Recent {
    /// key：按键；voice：语音
    pub kind: String,
    /// 按键的模块 id（语音没有），连发合并时认这个
    pub id: String,
    /// 按键是按键上的字，语音是「语音 · N 字」
    pub title: String,
    /// 按键是「单击 → Enter」「按住连发 → Backspace」，语音是说的话
    pub detail: String,
    /// 发生的时刻（Unix 秒）
    pub at: u64,
    /// 按住连发时同一个键连着按了几下，合成一条；界面上大于 1 才显示「×N」
    pub count: u32,
}

pub const RECENT_MAX: usize = 8;

fn app_data() -> PathBuf {
    PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into()))
}

pub fn data_dir() -> PathBuf {
    app_data().join("SuanNIAO")
}

/// 改名前（0.7.4 以前）的数据文件夹 `%APPDATA%\PhonePad` 搬成 `%APPDATA%\SuanNIAO`，新的已经有了就不动。
/// 主界面和后台启动时、用到数据文件夹之前先调。搬了返回怎么搬的（写日志用）
pub fn move_legacy_data_dir() -> Option<&'static str> {
    move_dir_once(&app_data().join("PhonePad"), &data_dir())
}

/// old 搬成 new（new 还没有时）：整个文件夹改名；改不了名（旧后台还开着日志）就复制一份，旧的留着
fn move_dir_once(old: &Path, new: &Path) -> Option<&'static str> {
    if new.exists() || !old.is_dir() {
        return None;
    }
    if std::fs::rename(old, new).is_ok() {
        return Some("改名成");
    }
    // 主界面和后台同时启动时，可能另一个刚搬完
    if new.exists() {
        return None;
    }
    // 先复制到临时名字，复制完再改成正式的：复制到一半退出了，下次还会重新搬
    let mut temp = new.as_os_str().to_owned();
    temp.push(format!(".moving-{}", std::process::id()));
    let temp = PathBuf::from(temp);
    if copy_dir(old, &temp).is_ok() && std::fs::rename(&temp, new).is_ok() {
        return Some("复制到");
    }
    let _ = std::fs::remove_dir_all(&temp);
    None
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

pub fn status_path() -> PathBuf {
    data_dir().join("status.json")
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 解析配置文件的内容；格式不对或者一页都没有返回 None。
/// 一步看不懂的动作不会让这里失败（见 actions::Step），只有整份文件不是合法的配置才返回 None。
pub fn parse(text: &str) -> Option<Config> {
    parse_checked(text).ok()
}

/// 同 parse，读不出来时说明原因（给界面的横幅看）
pub fn parse_checked(text: &str) -> Result<Config, String> {
    let text = text.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Err("文件是空的".into());
    }
    let mut config = serde_json::from_str::<Config>(text).map_err(|e| match e.classify() {
        serde_json::error::Category::Eof => format!("文件不完整（第 {} 行就结束了）", e.line()),
        serde_json::error::Category::Syntax => format!("第 {} 行附近格式不对", e.line()),
        _ => format!("第 {} 行附近的内容不对", e.line()),
    })?;
    if config.layout.pages.is_empty() {
        return Err("里面一页布局都没有".into());
    }
    config.normalize();
    Ok(config)
}

/// 读配置文件的结果
pub enum Loaded {
    Ok(Config),
    /// 还没有文件（第一次用）
    Missing,
    /// 有文件但读不出来：原因和原文
    Broken { message: String, text: String },
}

/// 读配置文件（不改文件）
pub fn read_config() -> Loaded {
    match std::fs::read(config_path()) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            match parse_checked(&text) {
                Ok(config) => Loaded::Ok(config),
                Err(message) => Loaded::Broken { message, text },
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Loaded::Missing,
        Err(e) => Loaded::Broken { message: format!("打不开：{e}"), text: String::new() },
    }
}

/// 读配置；没有文件就用全新的默认配置。读不出来时也不改文件、不覆盖：先把坏文件另存一份（带时间），
/// 再用最近一份能读的备份，没有备份才用默认配置。返回（配置，读不出来的话是什么情况）
pub fn load() -> (Config, Option<ConfigError>) {
    match read_config() {
        Loaded::Ok(config) => (config, None),
        Loaded::Missing => (Config::fresh(), None),
        Loaded::Broken { message, text } => {
            let broken = preserve_broken(&text).unwrap_or_default();
            let (config, using) = match latest_backup() {
                Some((config, name)) => (config, format!("备份 {name}")),
                None => (Config::fresh(), "默认布局".to_string()),
            };
            (config, Some(ConfigError { message, broken, using, at: now_secs() }))
        }
    }
}

/// 读配置文件，读不到或者格式不对就返回 None（不改文件）。改了再写回去之前用这个，免得用默认值盖掉用户的配置。
pub fn load_existing() -> Option<Config> {
    parse(&std::fs::read_to_string(config_path()).ok()?)
}

/// 读不出来的配置文件另存成 config.broken-时间.json（已经存过一模一样的就不再存），返回文件名。
/// 只是复制，原来的 config.json 不动
pub fn preserve_broken(text: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let dir = data_dir();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("config.broken") && name.ends_with(".json") && std::fs::read(entry.path()).is_ok_and(|b| b == text.as_bytes()) {
                return Some(name);
            }
        }
    }
    let stamp = local_stamp();
    let name = std::iter::once(format!("config.broken-{stamp}.json"))
        .chain((2..).map(|n| format!("config.broken-{stamp}-{n}.json")))
        .find(|n| !dir.join(n).exists())?;
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::write(dir.join(&name), text).ok()?;
    Some(name)
}

/// 本地时间比 UTC 早多少秒（文件名里的时间用本地时间）。由程序启动时设置，没设置是 0
static LOCAL_OFFSET: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

pub fn set_local_offset_secs(offset: i64) {
    LOCAL_OFFSET.store(offset, std::sync::atomic::Ordering::Relaxed);
}

/// 现在的本地时间，文件名用：20261029-071500
pub fn local_stamp() -> String {
    let secs = (now_secs() as i64 + LOCAL_OFFSET.load(std::sync::atomic::Ordering::Relaxed)).max(0) as u64;
    let iso = transfer::iso_utc(secs);
    // 2026-10-29T07:15:00Z → 20261029-071500
    iso.trim_end_matches('Z').replace(['-', ':'], "").replace('T', "-")
}

/// 现在的本地日期：20261029
pub fn local_date() -> String {
    local_stamp().chars().take(8).collect()
}

/// 备份放在这里：自动备份 config-auto-日期.json，导入前的 config-before-import-时间.json
pub fn backups_dir() -> PathBuf {
    data_dir().join("backups")
}

/// 自动备份留几天的
const AUTO_BACKUPS: usize = 7;

/// 配置读好了：记一份自动备份（每天一个文件，当天的随时更新，最多留 7 天），配置文件读坏时用来恢复
pub fn remember_good(text: &str) {
    let dir = backups_dir();
    let path = dir.join(format!("config-auto-{}.json", local_date()));
    if std::fs::read(&path).is_ok_and(|b| b == text.as_bytes()) {
        return;
    }
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
    // 只留最近几天的
    let mut autos: Vec<String> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("config-auto-") && n.ends_with(".json")).collect()
        })
        .unwrap_or_default();
    autos.sort();
    while autos.len() > AUTO_BACKUPS {
        let _ = std::fs::remove_file(dir.join(autos.remove(0)));
    }
}

/// 最近一份能读的备份（backups 文件夹里的，还有数据文件夹里手动备份的 config.backup*.json），按修改时间从新到旧找。
/// 返回（配置，文件名）
pub fn latest_backup() -> Option<(Config, String)> {
    let mut candidates: Vec<(SystemTime, PathBuf)> = Vec::new();
    for (dir, wanted) in [(backups_dir(), ""), (data_dir(), "config.backup")] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".json") && name.starts_with(wanted) {
                if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
                    candidates.push((modified, entry.path()));
                }
            }
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates.into_iter().find_map(|(_, path)| {
        let text = std::fs::read_to_string(&path).ok()?;
        // 导出的备份文件（suanniao-backup，改名前是 phonepad-backup）也认
        let config = parse(&text).or_else(|| transfer::parse_import(&text).ok().map(|(c, _)| c))?;
        Some((config, path.file_name()?.to_string_lossy().into_owned()))
    })
}

/// 配置写进文件时的样子（主界面靠比较这段文字分辨是不是自己写的）
pub fn to_text(config: &Config) -> String {
    serde_json::to_string_pretty(config).unwrap_or_default()
}

/// 主界面里有没保存的改动、文件又被别处改了（比如手机上调了布局）：三方合并，两边的改动都留下（见 merge::merge3）。
/// 合并出来的布局对不上（重叠、出界）返回 None。
pub fn merge_configs(base: &Config, mine: &Config, theirs: &Config) -> Option<Config> {
    let value = |c: &Config| serde_json::to_value(c).ok();
    let merged = merge::merge3(value(base).as_ref(), value(mine).as_ref(), value(theirs).as_ref())?;
    let config: Config = serde_json::from_value(merged).ok()?;
    (!config.layout.pages.is_empty() && config.check_layout().is_ok()).then_some(config)
}

/// 和 merge_configs 一样，但两边改了同一个地方就算合不了（返回 None）：撤销记录合进别处的改动时用
pub fn merge_configs_strict(base: &Config, mine: &Config, theirs: &Config) -> Option<Config> {
    let value = |c: &Config| serde_json::to_value(c).ok();
    let merged = merge::merge3_strict(value(base).as_ref(), value(mine).as_ref(), value(theirs).as_ref()).ok()??;
    let config: Config = serde_json::from_value(merged).ok()?;
    (!config.layout.pages.is_empty() && config.check_layout().is_ok()).then_some(config)
}

/// 先写临时文件再改名，后台不会读到写了一半的文件。
pub fn save(config: &Config) -> std::io::Result<()> {
    write_json(&config_path(), &to_text(config))
}

pub fn write_json(path: &PathBuf, text: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir())?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

pub fn read_status() -> Option<Status> {
    serde_json::from_str(&std::fs::read_to_string(status_path()).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧版（整数格子、没有连发字段）的配置照样能读，写回去整数还是整数
    #[test]
    fn old_config_still_loads() {
        let old = r#"{"version":1,"layout":{"columns":4,"rows":8,"pages":[{"id":"main","name":"主页","modules":[
            {"id":"up","kind":"button","label":"↑","x":2,"y":2,"w":1,"h":1,"behaviors":{"click":[{"type":"key","key":"Up"}]}}]}]},
            "settings":{"triggers":{"doubleClickMs":300,"longPressMs":500}}}"#;
        let mut config = parse(old).unwrap();
        let m = &config.layout.pages[0].modules[0];
        assert_eq!((m.x, m.y, m.w, m.h, m.repeat), (2.0, 2.0, 1.0, 1.0, false));
        assert_eq!(config.settings.triggers.repeat_delay_ms, 400);
        assert_eq!(config.settings.triggers.repeat_interval_ms, 50);
        let text = to_text(&config);
        assert!(text.contains("\"x\": 2,"), "{text}");
        config.layout.pages[0].modules[0].set_rect(Rect::new(2.25, 2.0, 1.5, 0.75));
        let text = to_text(&config);
        assert!(text.contains("\"x\": 2.25,") && text.contains("\"h\": 0.75,"), "{text}");
        assert!(config.check_layout().is_ok());
    }

    #[test]
    fn phone_layout_is_checked() {
        let mut config = Config::default();
        // 删除键往左上长大会压到换行键：整个不改
        let err = config.set_page_layout("main", &[("backspace".into(), Rect::new(2.5, 6.0, 1.5, 2.0))]).unwrap_err();
        assert!(err.contains("重叠"), "{err}");
        assert_eq!(config.find_module("backspace").unwrap().x, 3.0);
        // 换行键变窄、删除键往左长：可以
        let n = config
            .set_page_layout(
                "main",
                &[("newline".into(), Rect::new(2.0, 6.0, 0.75, 2.0)), ("backspace".into(), Rect::new(2.75, 6.0, 1.25, 2.0)), ("gone".into(), Rect::default())],
            )
            .unwrap();
        assert_eq!(n, 2);
        assert_eq!(config.find_module("backspace").unwrap().w, 1.25);
        assert!(config.set_page_layout("nope", &[]).is_err());
        // 默认布局的空位
        assert_eq!(config.free_spot(0, 1.0, 1.0), None);
        let payload = config.phone_payload();
        let backspace = &payload["layout"]["pages"][0]["modules"][12];
        assert_eq!(backspace["repeat"], true);
        assert_eq!(backspace["triggers"]["longPress"], false); // 开了连发，长按不发给手机
        assert_eq!(backspace["x"], 2.75);
        assert_eq!(payload["triggers"]["repeatIntervalMs"], 50);
    }

    /// 改名前的数据文件夹整个搬过来，只搬一次；复制的时候子文件夹也带上
    #[test]
    fn legacy_data_dir_moves_once() {
        let root = std::env::temp_dir().join(format!("suanniao-movetest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (old, new) = (root.join("PhonePad"), root.join("SuanNIAO"));
        std::fs::create_dir_all(old.join("logs")).unwrap();
        std::fs::write(old.join("config.json"), "{}").unwrap();
        std::fs::write(old.join("logs").join("core.log"), "log").unwrap();
        assert_eq!(move_dir_once(&old, &new), Some("改名成"));
        assert!(!old.exists());
        assert_eq!(std::fs::read_to_string(new.join("logs").join("core.log")).unwrap(), "log");
        // 新的已经有了：旧的再出现也不动
        std::fs::create_dir_all(&old).unwrap();
        assert_eq!(move_dir_once(&old, &new), None);
        assert!(old.exists());
        let copy = root.join("copy");
        copy_dir(&new, &copy).unwrap();
        assert_eq!(std::fs::read_to_string(copy.join("config.json")).unwrap(), "{}");
        assert_eq!(std::fs::read_to_string(copy.join("logs").join("core.log")).unwrap(), "log");
        let _ = std::fs::remove_dir_all(&root);
    }
}
