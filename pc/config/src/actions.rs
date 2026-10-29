//! 动作：按键按下后电脑要做的一串事。格式和 axonkey 的行为（behavior）一致，方便互相导入：
//! `{"id": "...", "enabled": true, "type": "key", "key": "Enter"}`。
//!
//! - 每一步有 id（没有就自动生成）和 enabled（没有算 true）；停用的步骤执行时跳过。
//! - 读旧配置时：`text` 当作 `paste`，`media` 换成对应的按键名（MediaPlayPause 等），
//!   以前放在「打开应用」里的网址换成「打开网站」。
//! - 看不懂的步骤（更新的版本写的新类型、或者字段格式不对）原样保留：保存时照原样写回，执行时跳过，
//!   界面上显示成只读的「看不懂的动作」。一步看不懂绝不会连累整份配置。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::Deserializer;
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// 动作序列里的一步
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub id: String,
    pub enabled: bool,
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// 按一下键（不分大小写的按键名，见 key_code）。空的是刚加、还没选按键的一步（执行时什么都不做）
    Key { key: String },
    /// 依次按下、倒序松开（可以只有修饰键）
    Shortcut { keys: Vec<String> },
    /// 鼠标单击：left / right / middle / back / forward
    Mouse { button: String },
    /// 滚轮一格（120）：up / down / left / right
    Wheel { direction: String },
    /// 光标按像素相对移动，1～500
    CursorMove { direction: String, distance: u32 },
    /// 把文字打到电脑上。mode：None 跟随设置里的「文字上屏方式」
    Paste { text: String, mode: Option<PasteMode> },
    /// 等一会儿再做后面的步骤（不卡别的按键），0～300000 毫秒
    Delay { ms: u64 },
    /// 打开程序或快捷方式。args：启动参数；activate：已经在运行就切到它的窗口
    OpenApp { path: String, args: String, activate: bool },
    /// 用系统默认浏览器打开网址（只认 http、https）
    OpenWebsite { url: String },
    /// 切到上一个 / 下一个方案（previous / next）；wrap：到头了从另一头接着切（默认）
    SwitchProfile { direction: String, wrap: bool },
    /// 什么都不做
    Disabled,
    /// 看不懂的一步：原样保留
    Unknown(Value),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteMode {
    /// 逐字输入（不碰剪贴板）
    Type,
    /// 放进剪贴板再 Ctrl+V
    Clipboard,
}

pub const MAX_DELAY_MS: u64 = 300_000;
pub const MAX_CURSOR_DISTANCE: u32 = 500;
pub const DEFAULT_CURSOR_DISTANCE: u32 = 50;

// ───────── 按键名 ─────────

/// 下拉框里的按键：（配置里的值，显示名），按 axonkey 的分组排
pub const KEY_GROUPS: [(&str, &[(&str, &str)]); 6] = [
    (
        "常用按键",
        &[
            ("Esc", "退出（Esc）"),
            ("Enter", "回车"),
            ("Space", "空格"),
            ("Tab", "切换焦点（Tab）"),
            ("Backspace", "向前删除（Backspace）"),
            ("Delete", "向后删除（Delete）"),
            ("Insert", "插入（Insert）"),
            ("Home", "跳到开头（Home）"),
            ("End", "跳到结尾（End）"),
            ("PageUp", "向上翻页（PageUp）"),
            ("PageDown", "向下翻页（PageDown）"),
            ("Up", "方向上"),
            ("Down", "方向下"),
            ("Left", "方向左"),
            ("Right", "方向右"),
            ("Apps", "菜单键（Apps）"),
        ],
    ),
    (
        "单独修饰键",
        &[
            ("Ctrl", "Ctrl"),
            ("RCtrl", "右 Ctrl"),
            ("Shift", "Shift"),
            ("RShift", "右 Shift"),
            ("Alt", "Alt"),
            ("LAlt", "左 Alt"),
            ("RAlt", "右 Alt"),
            ("Win", "Win"),
            ("RWin", "右 Win"),
        ],
    ),
    (
        "标点符号",
        &[
            ("[", "[  左方括号"),
            ("]", "]  右方括号"),
            ("\\", "\\  反斜杠"),
            (";", ";  分号"),
            ("'", "'  单引号"),
            (",", ",  逗号"),
            (".", ".  句点"),
            ("/", "/  斜杠"),
            ("-", "-  减号"),
            ("=", "=  等号"),
            ("`", "`  反引号"),
        ],
    ),
    (
        "媒体按键",
        &[
            ("VolumeUp", "增大音量"),
            ("VolumeDown", "减小音量"),
            ("VolumeMute", "静音"),
            ("MediaPlayPause", "播放 / 暂停"),
            ("MediaPrevious", "上一首"),
            ("MediaNext", "下一首"),
            ("MediaStop", "停止播放"),
        ],
    ),
    (
        "字母与数字",
        &[
            ("A", "A"), ("B", "B"), ("C", "C"), ("D", "D"), ("E", "E"), ("F", "F"), ("G", "G"), ("H", "H"), ("I", "I"),
            ("J", "J"), ("K", "K"), ("L", "L"), ("M", "M"), ("N", "N"), ("O", "O"), ("P", "P"), ("Q", "Q"), ("R", "R"),
            ("S", "S"), ("T", "T"), ("U", "U"), ("V", "V"), ("W", "W"), ("X", "X"), ("Y", "Y"), ("Z", "Z"), ("0", "0"),
            ("1", "1"), ("2", "2"), ("3", "3"), ("4", "4"), ("5", "5"), ("6", "6"), ("7", "7"), ("8", "8"), ("9", "9"),
        ],
    ),
    (
        "功能键",
        &[
            ("F1", "F1"), ("F2", "F2"), ("F3", "F3"), ("F4", "F4"), ("F5", "F5"), ("F6", "F6"), ("F7", "F7"), ("F8", "F8"),
            ("F9", "F9"), ("F10", "F10"), ("F11", "F11"), ("F12", "F12"), ("F13", "F13"), ("F14", "F14"), ("F15", "F15"),
            ("F16", "F16"), ("F17", "F17"), ("F18", "F18"), ("F19", "F19"), ("F20", "F20"), ("F21", "F21"), ("F22", "F22"),
            ("F23", "F23"), ("F24", "F24"),
        ],
    ),
];

/// 所有能在下拉框里选的按键（不含分组标题），顺序同上
pub fn key_list() -> Vec<(&'static str, &'static str)> {
    KEY_GROUPS.iter().flat_map(|(_, keys)| keys.iter().copied()).collect()
}

/// 按键名 → Windows 虚拟键码，不分大小写。和 axonkey 认的名字一样，另外认蒜鸟以前用过的几个名字。
/// 标点的 Shift 变体（: + < _ > ? ~ { | } "）和中文标点都按不带 Shift 的同一个键发（和 axonkey 一样）。
pub fn key_code(name: &str) -> Option<u16> {
    let name = name.trim();
    let upper = name.to_ascii_uppercase();
    let vk: u16 = match upper.as_str() {
        "CTRL" | "CONTROL" | "LCTRL" | "LCONTROL" => 0xA2,
        "RCTRL" | "RCONTROL" => 0xA3,
        "SHIFT" | "LSHIFT" => 0xA0,
        "RSHIFT" => 0xA1,
        "ALT" | "LALT" => 0xA4,
        "RALT" => 0xA5,
        "WIN" | "LWIN" | "META" => 0x5B,
        "RWIN" => 0x5C,
        "ESC" | "ESCAPE" => 0x1B,
        "ENTER" | "RETURN" => 0x0D,
        "SPACE" => 0x20,
        "TAB" => 0x09,
        "BACKSPACE" => 0x08,
        "DELETE" | "DEL" => 0x2E,
        "INSERT" | "INS" => 0x2D,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" | "PGUP" => 0x21,
        "PAGEDOWN" | "PGDN" => 0x22,
        "UP" | "ARROWUP" => 0x26,
        "DOWN" | "ARROWDOWN" => 0x28,
        "LEFT" | "ARROWLEFT" => 0x25,
        "RIGHT" | "ARROWRIGHT" => 0x27,
        "APPS" | "MENU" | "CONTEXTMENU" => 0x5D,
        "VOLUMEMUTE" => 0xAD,
        "VOLUMEDOWN" => 0xAE,
        "VOLUMEUP" => 0xAF,
        "MEDIANEXT" | "NEXTTRACK" => 0xB0,
        "MEDIAPREVIOUS" | "MEDIAPREV" | "PREVTRACK" => 0xB1,
        "MEDIASTOP" => 0xB2,
        "MEDIAPLAYPAUSE" | "PLAYPAUSE" => 0xB3,
        ";" | ":" => 0xBA,
        "=" | "+" => 0xBB,
        "," | "，" | "<" => 0xBC,
        "-" | "_" => 0xBD,
        "." | "。" | ">" => 0xBE,
        "/" | "?" | "？" => 0xBF,
        "`" | "~" => 0xC0,
        "[" | "{" | "【" => 0xDB,
        "\\" | "|" => 0xDC,
        "]" | "}" | "】" => 0xDD,
        "'" | "\"" => 0xDE,
        other => {
            let bytes = other.as_bytes();
            if bytes.len() == 1 && bytes[0].is_ascii_alphanumeric() {
                bytes[0] as u16
            } else if let Some(n) = other.strip_prefix('F').and_then(|s| s.parse::<u16>().ok()).filter(|n| (1..=24).contains(n)) {
                0x6F + n
            } else {
                return None;
            }
        }
    };
    Some(vk)
}

/// 修饰键（按住直通时引用计数、不补发按下）
pub fn is_modifier(vk: u16) -> bool {
    matches!(vk, 0x10..=0x12 | 0xA0..=0xA5 | 0x5B | 0x5C)
}

/// 组合键文字（Ctrl+Shift+P）拆成按键名；末尾的 "++" 表示加号键（Ctrl++）
pub fn split_shortcut(text: &str) -> Vec<String> {
    let trimmed = text.trim();
    let mut keys: Vec<String> = trimmed.split('+').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    if trimmed == "+" || trimmed.ends_with("++") {
        keys.push("+".into());
    }
    keys
}

/// 媒体控制（界面上单独一类，存成按键名）：（按键名，显示名）
pub const MEDIA_KEYS: [(&str, &str); 7] = [
    ("MediaPlayPause", "播放 / 暂停"),
    ("MediaNext", "下一首"),
    ("MediaPrevious", "上一首"),
    ("MediaStop", "停止播放"),
    ("VolumeUp", "增大音量"),
    ("VolumeDown", "减小音量"),
    ("VolumeMute", "静音"),
];

/// 鼠标键：（配置里的值，显示名）。back / forward 是侧键（XBUTTON1 / XBUTTON2），中键是蒜鸟多出来的
pub const MOUSE_BUTTONS: [(&str, &str); 5] =
    [("left", "左键"), ("right", "右键"), ("middle", "中键"), ("back", "后退键（侧键）"), ("forward", "前进键（侧键）")];
/// 滚轮、光标的方向
pub const DIRECTIONS: [(&str, &str); 4] = [("up", "上"), ("down", "下"), ("left", "左"), ("right", "右")];
/// 「输入文本」的方式（界面下拉框的顺序）：跟随设置里的「文字上屏方式」、逐字输入、放进剪贴板再粘贴
pub const PASTE_MODES: [&str; 3] = ["跟随设置", "逐字", "剪贴板"];

/// 界面上「动作类型」下拉框的顺序。0 按键 … 10 切换方案、11 禁用；看不懂的步骤是 -1（只读）
pub const ACTION_KINDS: [&str; 12] =
    ["按键", "组合键", "鼠标按键", "鼠标滚轮", "光标移动", "输入文本", "等待", "打开应用", "打开网站", "媒体控制", "切换方案", "禁用"];

fn lookup(table: &[(&str, &str)], value: &str) -> String {
    table.iter().find(|(code, _)| code.eq_ignore_ascii_case(value)).map(|(_, name)| name.to_string()).unwrap_or_else(|| value.to_string())
}

// ───────── 读写 JSON ─────────

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// 新步骤的 id：时间 + 计数，同一份配置里不会重复
pub fn new_step_id() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("step-{:x}{:04x}", nanos ^ ((std::process::id() as u64) << 40), n & 0xFFFF)
}

fn direction(value: Option<&Value>) -> Option<String> {
    let d = value?.as_str()?;
    DIRECTIONS.iter().any(|(code, _)| *code == d).then(|| d.to_string())
}

fn clamp_distance(value: Option<&Value>) -> Option<u32> {
    match value {
        None | Some(Value::Null) => Some(DEFAULT_CURSOR_DISTANCE),
        Some(v) => {
            let n = v.as_f64()?;
            let n = n.round();
            Some(if n < 1.0 { DEFAULT_CURSOR_DISTANCE } else { (n as u64).min(MAX_CURSOR_DISTANCE as u64) as u32 })
        }
    }
}

impl Step {
    pub fn new(action: Action) -> Self {
        Self { id: new_step_id(), enabled: true, action }
    }

    /// 从 JSON 读一步。看不懂的原样留成 Unknown（id 留空的话由 Config::normalize 按位置补上）
    pub fn from_value(value: Value) -> Self {
        let Value::Object(map) = &value else {
            return Self { id: String::new(), enabled: true, action: Action::Unknown(value) };
        };
        let id = map.get("id").and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
        let enabled = map.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        let action = Action::parse(map).unwrap_or_else(|| Action::Unknown(value.clone()));
        Self { id, enabled, action }
    }

    /// 写成 JSON：看得懂的按标准格式写，看不懂的原样写回
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("id".into(), json!(self.id));
        map.insert("enabled".into(), json!(self.enabled));
        let (kind, fields): (&str, Value) = match &self.action {
            Action::Unknown(raw) => return raw.clone(),
            Action::Key { key } => ("key", json!({"key": key})),
            Action::Shortcut { keys } => ("shortcut", json!({"keys": keys})),
            Action::Mouse { button } => ("mouse", json!({"button": button})),
            Action::Wheel { direction } => ("wheel", json!({"direction": direction})),
            Action::CursorMove { direction, distance } => ("cursorMove", json!({"direction": direction, "distance": distance})),
            Action::Paste { text, mode } => {
                let mut fields = json!({"text": text});
                if let Some(mode) = mode {
                    fields["mode"] = json!(match mode {
                        PasteMode::Type => "type",
                        PasteMode::Clipboard => "clipboard",
                    });
                }
                ("paste", fields)
            }
            Action::Delay { ms } => ("delay", json!({"ms": ms})),
            Action::OpenApp { path, args, activate } => {
                let mut fields = json!({"path": path});
                if !args.is_empty() {
                    fields["args"] = json!(args);
                }
                if *activate {
                    fields["activate"] = json!(true);
                }
                ("openApp", fields)
            }
            Action::OpenWebsite { url } => ("openWebsite", json!({"url": url})),
            Action::SwitchProfile { direction, wrap } => ("switchProfile", json!({"direction": direction, "wrap": wrap})),
            Action::Disabled => ("disabled", json!({})),
        };
        map.insert("type".into(), json!(kind));
        if let Value::Object(fields) = fields {
            map.extend(fields);
        }
        Value::Object(map)
    }

    /// 执行时会不会做事：启用、而且不是「禁用」、看不懂的、还没选按键的步骤
    pub fn active(&self) -> bool {
        self.enabled && !self.action.is_blank() && !matches!(self.action, Action::Disabled | Action::Unknown(_))
    }
}

impl Serialize for Step {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(s)
    }
}

impl<'de> Deserialize<'de> for Step {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Step::from_value(Value::deserialize(d)?))
    }
}

/// 一种按法里有没有会做事的步骤（只剩「禁用」或停用的步骤时，判定时当作没配）
pub fn any_active(steps: &[Step]) -> bool {
    steps.iter().any(Step::active)
}

impl Action {
    /// 按 type 解析；字段缺了或类型不对返回 None（留成 Unknown）
    fn parse(map: &Map<String, Value>) -> Option<Action> {
        let s = |k: &str| map.get(k).and_then(Value::as_str);
        Some(match map.get("type")?.as_str()? {
            // 空的也认（界面上刚加、还没选按键），执行时什么都不按
            "key" => Action::Key { key: s("key")?.trim().to_string() },
            "shortcut" => {
                let keys: Vec<String> = map.get("keys")?.as_array()?.iter().map(|k| k.as_str().map(|k| k.trim().to_string())).collect::<Option<_>>()?;
                // 空的也认（界面上刚清空还没填），执行时什么都不按
                Action::Shortcut { keys: keys.into_iter().filter(|k| !k.is_empty()).collect() }
            }
            "mouse" => {
                let button = s("button")?;
                MOUSE_BUTTONS.iter().any(|(code, _)| *code == button).then(|| Action::Mouse { button: button.to_string() })?
            }
            "wheel" => Action::Wheel { direction: direction(map.get("direction"))? },
            "cursorMove" => Action::CursorMove { direction: direction(map.get("direction"))?, distance: clamp_distance(map.get("distance"))? },
            // 旧版的「输入文本」就是粘贴文本，跟随设置
            "paste" | "text" => {
                let mode = match map.get("mode") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(m)) if m == "type" => Some(PasteMode::Type),
                    Some(Value::String(m)) if m == "clipboard" => Some(PasteMode::Clipboard),
                    Some(_) => return None,
                };
                Action::Paste { text: s("text")?.to_string(), mode }
            }
            "delay" => {
                let ms = match map.get("ms") {
                    None | Some(Value::Null) => 300.0,
                    Some(v) => v.as_f64()?,
                };
                Action::Delay { ms: ms.round().clamp(0.0, MAX_DELAY_MS as f64) as u64 }
            }
            "openApp" => {
                let path = s("path")?.trim().trim_matches('"').to_string();
                // 以前「打开应用」里也能填网址，现在网址归「打开网站」
                if path.to_ascii_lowercase().starts_with("http://") || path.to_ascii_lowercase().starts_with("https://") {
                    Action::OpenWebsite { url: path }
                } else {
                    let args = match map.get("args") {
                        None | Some(Value::Null) => String::new(),
                        Some(v) => v.as_str()?.to_string(),
                    };
                    let activate = match map.get("activate") {
                        None | Some(Value::Null) => false,
                        Some(v) => v.as_bool()?,
                    };
                    Action::OpenApp { path, args, activate }
                }
            }
            "openWebsite" => Action::OpenWebsite { url: s("url")?.trim().to_string() },
            "switchProfile" => {
                let direction = s("direction")?;
                if direction != "previous" && direction != "next" {
                    return None;
                }
                let wrap = match map.get("wrap") {
                    None | Some(Value::Null) => true,
                    Some(v) => v.as_bool()?,
                };
                Action::SwitchProfile { direction: direction.to_string(), wrap }
            }
            "disabled" => Action::Disabled,
            // 旧版的「媒体控制」：换成对应的按键名
            "media" => {
                let key = match s("action")? {
                    "playPause" => "MediaPlayPause",
                    "next" => "MediaNext",
                    "prev" => "MediaPrevious",
                    "stop" => "MediaStop",
                    "volumeUp" => "VolumeUp",
                    "volumeDown" => "VolumeDown",
                    "mute" => "VolumeMute",
                    _ => return None,
                };
                Action::Key { key: key.into() }
            }
            _ => return None,
        })
    }

    /// 界面上「动作类型」下拉框里的位置（-1：看不懂）
    pub fn kind_index(&self) -> i32 {
        match self {
            Action::Key { key } if MEDIA_KEYS.iter().any(|(k, _)| k.eq_ignore_ascii_case(key)) => 9,
            Action::Key { .. } => 0,
            Action::Shortcut { .. } => 1,
            Action::Mouse { .. } => 2,
            Action::Wheel { .. } => 3,
            Action::CursorMove { .. } => 4,
            Action::Paste { .. } => 5,
            Action::Delay { .. } => 6,
            Action::OpenApp { .. } => 7,
            Action::OpenWebsite { .. } => 8,
            Action::SwitchProfile { .. } => 10,
            Action::Disabled => 11,
            Action::Unknown(_) => -1,
        }
    }

    /// 还没填好、执行时什么都不做的按键或组合键（界面上标红「请选择」）
    pub fn is_blank(&self) -> bool {
        match self {
            Action::Key { key } => key.is_empty(),
            Action::Shortcut { keys } => keys.is_empty(),
            _ => false,
        }
    }

    /// 新建某种类型的动作时的默认值：按键先空着（标红让人选），免得忘了改就按出回车
    pub fn default_of_kind(index: usize) -> Action {
        match index {
            1 => Action::Shortcut { keys: vec!["Ctrl".into(), "C".into()] },
            2 => Action::Mouse { button: "left".into() },
            3 => Action::Wheel { direction: "down".into() },
            4 => Action::CursorMove { direction: "up".into(), distance: DEFAULT_CURSOR_DISTANCE },
            5 => Action::Paste { text: String::new(), mode: None },
            6 => Action::Delay { ms: 300 },
            7 => Action::OpenApp { path: String::new(), args: String::new(), activate: false },
            8 => Action::OpenWebsite { url: String::new() },
            9 => Action::Key { key: "MediaPlayPause".into() },
            10 => Action::SwitchProfile { direction: "next".into(), wrap: true },
            11 => Action::Disabled,
            _ => Action::Key { key: String::new() },
        }
    }

    /// 只是一个按键或组合键（按住直通要求这样），返回按键名
    pub fn chord(&self) -> Option<Vec<String>> {
        let keys = match self {
            Action::Key { key } => vec![key.clone()],
            Action::Shortcut { keys } => keys.clone(),
            _ => return None,
        };
        (!keys.is_empty() && keys.iter().all(|k| key_code(k).is_some())).then_some(keys)
    }

    /// 总览和日志里显示的简短说明
    pub fn summary(&self) -> String {
        match self {
            Action::Key { key } if key.is_empty() => "（没选按键）".into(),
            Action::Key { key } => MEDIA_KEYS.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, n)| n.to_string()).unwrap_or_else(|| key.clone()),
            Action::Shortcut { keys } if keys.is_empty() => "（没填组合键）".into(),
            Action::Shortcut { keys } => keys.join("+"),
            Action::Mouse { button } => format!("鼠标{}", lookup(&MOUSE_BUTTONS, button)),
            Action::Wheel { direction } => format!("滚轮{}", lookup(&DIRECTIONS, direction)),
            Action::CursorMove { direction, distance } => format!("光标{}移 {distance}", lookup(&DIRECTIONS, direction)),
            Action::Paste { text, .. } => {
                let shown: String = text.chars().take(12).collect();
                format!("输入「{}{}」", shown.replace('\n', " "), if text.chars().count() > 12 { "…" } else { "" })
            }
            Action::Delay { ms } => format!("等 {ms} 毫秒"),
            Action::OpenApp { path, .. } => {
                let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
                let name = name.strip_suffix(".lnk").or_else(|| name.strip_suffix(".exe")).unwrap_or(name);
                format!("打开 {}", if name.is_empty() { "（未设置）" } else { name })
            }
            Action::OpenWebsite { url } => {
                let host = url.split("://").nth(1).unwrap_or(url).split(['/', '?', '#']).next().unwrap_or("");
                format!("打开 {}", if host.is_empty() { "（未设置）" } else { host })
            }
            Action::SwitchProfile { direction, wrap } => format!(
                "{}{}",
                if direction == "previous" { "切到上一个方案" } else { "切到下一个方案" },
                if *wrap { "" } else { "（到头停）" }
            ),
            Action::Disabled => "禁用".into(),
            Action::Unknown(raw) => format!("看不懂的动作（{}）", raw.get("type").and_then(Value::as_str).unwrap_or("?")),
        }
    }
}

/// 一串动作的简短说明（停用的步骤不列）
pub fn summarize(steps: &[Step]) -> String {
    let parts: Vec<String> = steps.iter().filter(|s| s.enabled).map(|s| s.action.summary()).collect();
    if parts.is_empty() { "—".into() } else { parts.join(" → ") }
}

// ───────── 校验 ─────────

/// 网址：只认 http、https；没写协议就补 https://；不能有空白、控制字符、账号密码。不合法返回 None
pub fn normalize_url(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() || s.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    // 「字母开头: 」算写了协议，但 localhost:3000 这种主机加端口不算
    let colon = s.find(':');
    let scheme_like = colon.is_some_and(|i| {
        let head = &s[..i];
        !head.is_empty() && head.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && head.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
    });
    let host_port = colon.is_some_and(|i| {
        let head = &s[..i];
        let rest = &s[i + 1..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        !head.contains(['/', '?', '#']) && !digits.is_empty() && rest[digits.len()..].chars().next().is_none_or(|c| "/?#".contains(c))
    });
    let full = if scheme_like && !host_port { s.to_string() } else { format!("https://{s}") };
    let lower = full.to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &full[8..]
    } else if lower.starts_with("http://") {
        &full[7..]
    } else {
        return None;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let host = if authority.starts_with('[') {
        authority.split(']').next().map(|h| format!("{h}]")).unwrap_or_default()
    } else {
        let mut parts = authority.splitn(2, ':');
        let host = parts.next().unwrap_or("").to_string();
        if let Some(port) = parts.next() {
            if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
        }
        host
    };
    if host.is_empty() || host.starts_with('.') || host.contains("..") {
        return None;
    }
    let scheme = &lower[..lower.find("://").unwrap_or(0)];
    let tail = &rest[authority.len()..];
    Some(format!("{scheme}://{authority}{}", if tail.is_empty() { "/" } else { tail }))
}

/// 应用路径的问题（界面上的红字）；没问题返回 None。路径要写全（带盘符），文件要存在
pub fn app_path_problem(path: &str) -> Option<&'static str> {
    let path = path.trim().trim_matches('"');
    if path.is_empty() {
        return Some("还没选程序");
    }
    if path.chars().any(char::is_control) {
        return Some("路径里有看不见的字符");
    }
    let absolute = (path.len() > 2 && path.as_bytes()[1] == b':' && path.as_bytes()[0].is_ascii_alphabetic()) || path.starts_with("\\\\");
    if !absolute {
        return Some("要写完整路径（带盘符）");
    }
    if !std::path::Path::new(path).exists() {
        return Some("找不到这个文件");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(normalize_url("example.com").as_deref(), Some("https://example.com/"));
        assert_eq!(normalize_url("https://example.com/?a=1&b=%22").as_deref(), Some("https://example.com/?a=1&b=%22"));
        assert_eq!(normalize_url("HTTP://Example.com").as_deref(), Some("http://Example.com/"));
        assert_eq!(normalize_url("localhost:3000").as_deref(), Some("https://localhost:3000/"));
        assert_eq!(normalize_url("http://localhost:3000/x").as_deref(), Some("http://localhost:3000/x"));
        for bad in ["", "javascript:alert(1)", "file:///C:/a.exe", "https://user:pass@example.com", "https://exa mple.com/", "https://", "a b.com", "ftp://x.com"] {
            assert_eq!(normalize_url(bad), None, "{bad}");
        }
    }

    #[test]
    fn keys() {
        for name in ["Ctrl", "control", "LCtrl", "RWin", "Return", "ArrowUp", "，", "【", "?", "F13", "F24", "a", "Z", "0", "MediaStop", "playpause", "Apps"] {
            assert!(key_code(name).is_some(), "{name}");
        }
        assert_eq!(key_code("F25"), None);
        assert_eq!(key_code("ArrowUp"), key_code("Up"));
        assert_eq!(split_shortcut("Ctrl+Shift+P"), vec!["Ctrl", "Shift", "P"]);
        assert_eq!(split_shortcut("Ctrl++"), vec!["Ctrl", "+"]);
        assert_eq!(key_list().len(), 16 + 9 + 11 + 7 + 36 + 24);
    }

    #[test]
    fn steps_roundtrip_and_legacy() {
        let legacy = serde_json::json!([
            {"type": "text", "text": "hi"},
            {"type": "media", "action": "playPause"},
            {"type": "openApp", "path": "https://example.com"},
            {"type": "key", "key": "Enter"},
            {"type": "teleport", "where": "moon"},
            {"type": "mouse", "button": "nose"},
            {"type": "cursorMove", "direction": "up", "distance": 9999},
            "garbage",
        ]);
        let steps: Vec<Step> = serde_json::from_value(legacy).unwrap();
        assert_eq!(steps[0].action, Action::Paste { text: "hi".into(), mode: None });
        assert_eq!(steps[1].action, Action::Key { key: "MediaPlayPause".into() });
        assert_eq!(steps[2].action, Action::OpenWebsite { url: "https://example.com".into() });
        assert!(matches!(steps[4].action, Action::Unknown(_)));
        assert!(matches!(steps[5].action, Action::Unknown(_)));
        assert_eq!(steps[6].action, Action::CursorMove { direction: "up".into(), distance: 500 });
        assert!(matches!(steps[7].action, Action::Unknown(_)));
        // 看不懂的原样写回
        let back = serde_json::to_value(&steps).unwrap();
        assert_eq!(back[4], serde_json::json!({"type": "teleport", "where": "moon"}));
        assert_eq!(back[7], serde_json::json!("garbage"));
        assert_eq!(back[0]["type"], "paste");
        assert!(!any_active(&[Step { id: "a".into(), enabled: true, action: Action::Disabled }, Step { id: "b".into(), enabled: false, action: Action::Key { key: "A".into() } }]));
    }
}
