//! 从 axonkey 导入映射：转成蒜鸟的一页「遥控器」，13 个 1×1 的按键，位置和小米遥控器差不多。
//!
//! 能读的来源：
//! - axonkey 导出的映射文件 `{"format":"axonkey-mapping","version":1,"behaviors":{...}}`；
//! - axonkey 的数据备份 `{"format":"axonkey-backup","version":1,"profiles":[...]}`（取当前配置）；
//! - 更早的旧格式 `{"mappings":{"power":"Ctrl+C"}}`；
//! - 本机 axonkey 的数据（localStorage 里的 `axonkey.mapping-profiles.v1`，由 leveldb.rs 读出来）。
//!
//! 动作格式和 axonkey 一样，直接按 Step 读（看不懂的原样留着、不执行）。
//! 只读 axonkey 的数据，绝不写。

use serde_json::{Map, Value};

use crate::{any_active, Action, Behaviors, Config, HoldMode, Module, ModuleKind, Page, Step, Trigger};

/// 遥控器的 13 个键：（axonkey 里的名字，键上的字，位置，遥控器原来的按键）。
/// 单击为空但配了双击或长按时，单击补成原来的按键（电源键除外），和 axonkey 一样
pub const REMOTE_KEYS: [(&str, &str, (f64, f64), Option<&str>); 13] = [
    ("power", "电源", (0.0, 0.0), None),
    ("voice", "语音", (2.0, 0.0), Some("F5")),
    ("up", "↑", (1.0, 1.0), Some("Up")),
    ("left", "←", (0.0, 2.0), Some("Left")),
    ("confirm", "确认", (1.0, 2.0), Some("Enter")),
    ("right", "→", (2.0, 2.0), Some("Right")),
    ("down", "↓", (1.0, 3.0), Some("Down")),
    ("back", "返回", (0.0, 4.0), Some("Backspace")),
    ("volumeUp", "音量+", (2.0, 4.0), Some("VolumeUp")),
    ("home", "主页", (0.0, 5.0), Some("Home")),
    ("volumeDown", "音量−", (2.0, 5.0), Some("VolumeDown")),
    ("menu", "菜单", (0.0, 6.0), Some("Apps")),
    ("tv", "电视", (2.0, 6.0), Some("`")),
];

/// 读出来的一套 axonkey 映射，转换好了，等用户在预览里勾选
#[derive(Clone, Debug)]
pub struct Import {
    /// 原来软件里方案的名字（可能是空的）。导入成新方案时新方案叫「遥控器映射」，不用这个名字
    pub profile: String,
    pub keys: Vec<ImportKey>,
}

#[derive(Clone, Debug)]
pub struct ImportKey {
    pub id: &'static str,
    pub label: &'static str,
    pub behaviors: Behaviors,
    pub hold: HoldMode,
    /// 单击是补上的遥控器原键
    pub filled_click: bool,
    /// 三种按法有没有配（都没配的预览里默认不勾）
    pub configured: bool,
}

fn record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

/// 解析 axonkey 的文件（或本机数据），取出遥控器 13 个键的配置
pub fn parse(text: &str) -> Result<Import, String> {
    let root: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|_| "不是有效的 JSON 文件".to_string())?;
    let root = record(&root).ok_or("文件内容不是遥控器映射")?;
    let (profile, buttons) = match root.get("format").and_then(Value::as_str) {
        Some("axonkey-mapping") => {
            let behaviors = root.get("behaviors").and_then(record).ok_or("映射文件里没有按键配置")?;
            let name = root.get("profileName").and_then(Value::as_str).unwrap_or("导出的映射");
            (name.to_string(), behaviors.clone())
        }
        Some("axonkey-backup") => {
            let profiles = root.get("profiles").and_then(Value::as_array).ok_or("备份里没有方案")?;
            let active = root.get("activeProfileId").and_then(Value::as_str).unwrap_or("");
            let profile = profiles
                .iter()
                .find(|p| p.get("id").and_then(Value::as_str) == Some(active))
                .or_else(|| profiles.first())
                .ok_or("备份里没有方案")?;
            let keys = profile.pointer("/devices/rc003/keys").and_then(record).ok_or("备份里没有遥控器的按键")?;
            (profile.get("name").and_then(Value::as_str).unwrap_or("备份").to_string(), keys.clone())
        }
        Some(_) => return Err("不是遥控器映射文件".into()),
        None => {
            if let Some(profiles) = root.get("profiles").and_then(Value::as_array) {
                // 本机数据 axonkey.mapping-profiles.v1：{activeProfileId, profiles:[{id, name, behaviors}]}
                let active = root.get("activeProfileId").and_then(Value::as_str).unwrap_or("");
                let profile = profiles
                    .iter()
                    .find(|p| p.get("id").and_then(Value::as_str) == Some(active))
                    .or_else(|| profiles.first())
                    .ok_or("里面没有方案")?;
                let behaviors = profile.get("behaviors").and_then(record).ok_or("这个方案是空的")?;
                (profile.get("name").and_then(Value::as_str).unwrap_or("").to_string(), behaviors.clone())
            } else if let Some(mappings) = root.get("mappings").and_then(record) {
                ("旧版映射".to_string(), legacy(mappings))
            } else if let Some(behaviors) = root.get("behaviors").and_then(record) {
                (String::new(), behaviors.clone())
            } else if REMOTE_KEYS.iter().any(|(id, ..)| root.get(*id).is_some_and(Value::is_string)) {
                ("旧版映射".to_string(), legacy(root))
            } else {
                return Err("文件里找不到遥控器按键的配置".into());
            }
        }
    };
    if !REMOTE_KEYS.iter().any(|(id, ..)| buttons.contains_key(*id)) {
        return Err("文件里没有遥控器按键的配置".into());
    }
    let keys = REMOTE_KEYS
        .iter()
        .map(|&(id, label, _, original)| convert(id, label, original, buttons.get(id)))
        .collect();
    Ok(Import { profile, keys })
}

/// 旧格式：每个键一个字符串，比如 "Ctrl+C"（"original" 表示不改）
fn legacy(mappings: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (id, value) in mappings {
        let Some(text) = value.as_str().map(str::trim).filter(|t| !t.is_empty() && *t != "original") else { continue };
        let keys: Vec<&str> = text.split('+').map(str::trim).filter(|k| !k.is_empty()).collect();
        let step = if keys.len() > 1 {
            serde_json::json!({"id": format!("{id}-click-legacy"), "type": "shortcut", "keys": keys})
        } else {
            serde_json::json!({"id": format!("{id}-click-legacy"), "type": "key", "key": keys.first().copied().unwrap_or(text)})
        };
        out.insert(id.clone(), serde_json::json!({"click": [step]}));
    }
    out
}

fn convert(id: &'static str, label: &'static str, original: Option<&str>, raw: Option<&Value>) -> ImportKey {
    let list = |trigger: &str| -> Vec<Step> {
        raw.and_then(|r| r.get(trigger)).and_then(Value::as_array).map(|a| a.iter().cloned().map(Step::from_value).collect()).unwrap_or_default()
    };
    let mut behaviors = Behaviors { click: list("click"), double_click: list("doubleClick"), long_press: list("longPress"), extra: Map::new() };
    // 本身没有 id 的步骤按位置补，和 axonkey 读配置时一样
    for trigger in Trigger::ALL {
        for (i, step) in behaviors.get_mut(trigger).iter_mut().enumerate() {
            if step.id.is_empty() {
                step.id = format!("rc-{id}-{}-{}", trigger.code(), i + 1);
            }
        }
    }
    let configured = any_active(&behaviors.click) || any_active(&behaviors.double_click) || any_active(&behaviors.long_press);
    let others = any_active(&behaviors.double_click) || any_active(&behaviors.long_press);
    // 单击里一步启用的都没有（「禁用」也算启用，放了它就不补），又配了双击或长按：补成遥控器原来的键
    let mut filled_click = false;
    if !behaviors.click.iter().any(|s| s.enabled) && others {
        if let Some(key) = original {
            behaviors.click = vec![Step { id: format!("rc-{id}-click-original"), enabled: true, action: Action::Key { key: key.into() } }];
            filled_click = true;
        }
    }
    // 按住时：照 axonkey 的隐式规则
    let module = Module { kind: ModuleKind::Button, behaviors: behaviors.clone(), ..Default::default() };
    let hold = if any_active(&behaviors.long_press) {
        HoldMode::RepeatLongPress
    } else if any_active(&behaviors.double_click) {
        HoldMode::LongPress
    } else if module.passthrough_keys().is_some() {
        HoldMode::Passthrough
    } else {
        module.default_hold()
    };
    ImportKey { id, label, behaviors, hold, filled_click, configured }
}

/// 导入后新建的那一页叫什么：「遥控器」，已经有了就「遥控器 2」……
pub fn page_name(config: &Config) -> String {
    let names: Vec<&str> = config.layout.pages.iter().map(|p| p.name.as_str()).collect();
    std::iter::once("遥控器".to_string()).chain((2..).map(|n| format!("遥控器 {n}"))).find(|n| !names.contains(&n.as_str())).unwrap()
}

impl Import {
    /// 把勾选的键做成新的一页（名字见 page_name）。网格至少要 3 列 7 行
    pub fn build_page(&self, config: &Config, selected: &[bool]) -> Result<Page, String> {
        if config.layout.columns < 3 || config.layout.rows < 7 {
            return Err("网格太小：遥控器要 3 列 7 行，先在「布局」里把网格调大".into());
        }
        let name = page_name(config);
        let mut used: Vec<String> = config.layout.pages.iter().flat_map(|p| p.modules.iter().map(|m| m.id.clone())).collect();
        let mut modules = Vec::new();
        // keys 和 REMOTE_KEYS 的顺序一样，selected 也是
        for (i, (key, &(_, _, (x, y), _))) in self.keys.iter().zip(REMOTE_KEYS.iter()).enumerate() {
            if !selected.get(i).copied().unwrap_or(false) {
                continue;
            }
            let base = format!("rc-{}", key.id);
            let id = std::iter::once(base.clone()).chain((2..).map(|n| format!("{base}-{n}"))).find(|c| !used.contains(c)).unwrap();
            used.push(id.clone());
            let mut module = Module {
                id,
                kind: ModuleKind::Button,
                label: key.label.into(),
                x,
                y,
                w: 1.0,
                h: 1.0,
                behaviors: key.behaviors.clone(),
                ..Default::default()
            };
            module.set_hold(key.hold);
            modules.push(module);
        }
        if modules.is_empty() {
            return Err("一个键都没勾".into());
        }
        Ok(Page { id: config.new_id("remote"), name, modules })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用户本机 axonkey 的映射（2026-10-29 提取，见调研文档 §4）
    const USER: &str = r#"{"version":1,"activeProfileId":"p1","profiles":[{"id":"p1","name":"默认配置","behaviors":{
        "power":{"click":[{"id":"a","enabled":true,"type":"key","key":"Enter"}],"doubleClick":[],"longPress":[]},
        "voice":{"click":[{"id":"b","enabled":true,"type":"shortcut","keys":["Ctrl","."]}],"doubleClick":[],"longPress":[]},
        "up":{"click":[{"id":"c","enabled":true,"type":"key","key":"Up"}],"doubleClick":[{"id":"d","enabled":true,"type":"cursorMove","direction":"up","distance":50}],"longPress":[]},
        "confirm":{"click":[{"id":"e","enabled":true,"type":"mouse","button":"left"}],"doubleClick":[{"id":"f","enabled":true,"type":"mouse","button":"right"}],"longPress":[]},
        "home":{"click":[{"id":"g","enabled":true,"type":"openApp","path":"C:\\Users\\Administrator\\DeskBox\\AI\\ZCode.lnk"}],"doubleClick":[{"id":"h","enabled":true,"type":"openApp","path":"C:\\Users\\Administrator\\DeskBox\\AI\\ChatGPT.lnk"}],"longPress":[{"id":"i","enabled":true,"type":"shortcut","keys":["Win","D"]}]},
        "menu":{"click":[],"doubleClick":[],"longPress":[]},
        "tv":{"click":[{"id":"j","enabled":true,"type":"shortcut","keys":["Shift","Enter"]}],"doubleClick":[],"longPress":[]},
        "back":{"click":[],"doubleClick":[],"longPress":[{"id":"k","enabled":true,"type":"switchProfile","direction":"next","wrap":true}]}
    }}]}"#;

    #[test]
    fn converts_user_mapping() {
        let import = parse(USER).unwrap();
        assert_eq!(import.profile, "默认配置"); // 原来软件里的名字，只读不用
        let key = |id: &str| import.keys.iter().find(|k| k.id == id).unwrap();
        assert_eq!(key("power").hold, HoldMode::Passthrough);
        assert_eq!(key("voice").hold, HoldMode::Passthrough);
        assert_eq!(key("up").hold, HoldMode::LongPress); // 配了双击
        assert_eq!(key("home").hold, HoldMode::RepeatLongPress);
        assert_eq!(key("confirm").hold, HoldMode::LongPress);
        assert!(!key("menu").configured);
        assert!(key("tv").configured);
        // 切换方案（0.5.0 起认得）：长按配了，单击补成遥控器原来的返回键
        let back = key("back");
        assert!(matches!(back.behaviors.long_press[0].action, Action::SwitchProfile { .. }));
        assert!(back.filled_click);
        assert_eq!(back.hold, HoldMode::RepeatLongPress);
        let mut config = Config::default();
        let page = import.build_page(&config, &import.keys.iter().map(|k| k.configured).collect::<Vec<_>>()).unwrap();
        assert_eq!(page.name, "遥控器");
        assert!(page.modules.iter().any(|m| m.id == "rc-home" && m.x == 0.0 && m.y == 5.0));
        config.layout.pages.push(page);
        assert!(config.check_layout().is_ok());
        let again = import.build_page(&config, &[true; 13]).unwrap();
        assert_eq!(again.name, "遥控器 2");
        assert!(again.modules.iter().any(|m| m.id == "rc-home-2"));
    }

    #[test]
    fn fills_click_and_reads_other_formats() {
        let export = r#"{"format":"axonkey-mapping","version":1,"exportedAt":"x","enabled":true,"profileName":"导出",
            "behaviors":{"up":{"click":[],"doubleClick":[],"longPress":[{"type":"wheel","direction":"up"}]},
                         "down":{"click":[{"type":"disabled"}],"doubleClick":[{"type":"key","key":"A"}],"longPress":[]}}}"#;
        let import = parse(export).unwrap();
        let up = import.keys.iter().find(|k| k.id == "up").unwrap();
        assert!(up.filled_click);
        assert_eq!(up.behaviors.click[0].action, Action::Key { key: "Up".into() });
        let down = import.keys.iter().find(|k| k.id == "down").unwrap();
        assert!(!down.filled_click); // 单击里放了「禁用」就不补
        let old = parse(r#"{"mappings":{"power":"Ctrl+C","voice":"original","tv":"F5"}}"#).unwrap();
        let power = old.keys.iter().find(|k| k.id == "power").unwrap();
        assert_eq!(power.behaviors.click[0].action, Action::Shortcut { keys: vec!["Ctrl".into(), "C".into()] });
        assert!(!old.keys.iter().find(|k| k.id == "voice").unwrap().configured);
        // 数据备份：取当前方案
        let backup = r#"{"format":"axonkey-backup","version":1,"activeProfileId":"b","profiles":[
            {"id":"a","name":"甲","devices":{"rc003":{"keys":{"power":{"click":[{"type":"key","key":"A"}]}}}}},
            {"id":"b","name":"乙","devices":{"rc003":{"keys":{"power":{"click":[{"type":"key","key":"B"}]}}}}}]}"#;
        let backup = parse(backup).unwrap();
        assert_eq!(backup.profile, "乙");
        assert_eq!(backup.keys[0].behaviors.click[0].action, Action::Key { key: "B".into() });
        assert!(parse(r#"{"format":"something-else"}"#).is_err());
        assert!(parse("not json").is_err());
    }
}
