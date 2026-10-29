//! 导出、导入蒜鸟的配置。
//!
//! - 导出备份：`{"format":"suanniao-backup","version":1,"exportedAt","appVersion","config":{…}}`，配置和已保存的序列都在；
//! - 以前的「导出配置」`{"format":"suanniao-config",…}` 不含已保存的序列，导入时照样认；
//! - 改名前（PhonePad）导出的 `phonepad-backup`、`phonepad-config` 也认；
//! - 导入：上面两种，或者直接是 config.json。先整份校验，有一处不合法就整份拒绝、什么都不改。

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::actions::{key_code, normalize_url, Action, Step};
use crate::{clean_profile_name, Config, HoldMode, ModuleKind, Trigger};

pub const CONFIG_FORMAT: &str = "suanniao-config";
pub const BACKUP_FORMAT: &str = "suanniao-backup";
/// 改名前（PhonePad）导出的文件写的格式名
const OLD_CONFIG_FORMAT: &str = "phonepad-config";
const OLD_BACKUP_FORMAT: &str = "phonepad-backup";

/// UNIX 秒 → `2026-10-29T07:15:00Z`
pub fn iso_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // 公历日期（Howard Hinnant 的 civil_from_days）
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// `2026-10-29T07:15:00Z` → UNIX 秒（认不出来返回 None）
pub fn parse_iso_utc(text: &str) -> Option<u64> {
    let text = text.trim().trim_end_matches('Z');
    let (date, time) = text.split_once('T')?;
    let mut d = date.split('-').map(|v| v.parse::<i64>());
    let (year, month, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let mut t = time.split(':').map(|v| v.split('.').next().unwrap_or("0").parse::<i64>());
    let (hour, minute, second) = (t.next()?.ok()?, t.next()?.ok()?, t.next().and_then(|v| v.ok()).unwrap_or(0));
    // 公历日期 → 天数（Howard Hinnant 的 days_from_civil）
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hour * 3600 + minute * 60 + second).ok()
}

/// 导出整份配置（backup = true 时连已保存的序列一起）
pub fn export_document(config: &Config, backup: bool, app_version: &str) -> String {
    let mut config = config.clone();
    config.normalize();
    if !backup {
        config.saved_sequences.clear();
    }
    let document = json!({
        "format": if backup { BACKUP_FORMAT } else { CONFIG_FORMAT },
        "version": 1,
        "exportedAt": iso_utc(crate::now_secs()),
        "appVersion": app_version,
        "config": config,
    });
    serde_json::to_string_pretty(&document).unwrap_or_default()
}

/// 导入前给用户看的摘要
#[derive(Clone, Debug, Default)]
pub struct ImportSummary {
    /// 是数据备份（含已保存的序列）
    pub backup: bool,
    pub exported_at: String,
    pub app_version: String,
    pub profiles: Vec<String>,
    pub active: String,
    pub pages: usize,
    pub modules: usize,
    pub buttons: usize,
    pub sequences: usize,
}

/// 读导入的文件：整份校验，合法的话返回规整好的配置和摘要；有一处不合法返回说明（什么都不改）
pub fn parse_import(text: &str) -> Result<(Config, ImportSummary), String> {
    let root: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| format!("不是有效的 JSON（第 {} 行附近）", e.line()))?;
    let object = root.as_object().ok_or("文件内容不是蒜鸟的配置")?;
    let mut summary = ImportSummary::default();
    let raw = match object.get("format").and_then(Value::as_str) {
        Some(format @ (CONFIG_FORMAT | BACKUP_FORMAT | OLD_CONFIG_FORMAT | OLD_BACKUP_FORMAT)) => {
            if object.get("version").and_then(Value::as_u64) != Some(1) {
                return Err("不认识这个版本的导出文件".into());
            }
            summary.backup = format == BACKUP_FORMAT || format == OLD_BACKUP_FORMAT;
            summary.exported_at = object.get("exportedAt").and_then(Value::as_str).unwrap_or("").to_string();
            summary.app_version = object.get("appVersion").and_then(Value::as_str).unwrap_or("").to_string();
            object.get("config").cloned().ok_or("导出文件里没有配置")?
        }
        Some(format) if format.starts_with("axonkey") => {
            return Err("这是遥控器映射文件，不是蒜鸟的备份：请用「导入遥控器映射…」".into());
        }
        Some(_) => return Err("不认识这个文件的格式".into()),
        None => root.clone(),
    };
    check_raw(&raw)?;
    let mut config: Config = serde_json::from_value(raw).map_err(|e| format!("配置的格式不对：{e}"))?;
    config.normalize();
    check_config(&config)?;
    summary.profiles = config.profiles.iter().map(|p| p.name.clone()).collect();
    summary.active = config.active_profile_name();
    summary.pages = config.layout.pages.len();
    let modules: Vec<_> = config.layout.pages.iter().flat_map(|p| p.modules.iter()).collect();
    summary.modules = modules.len();
    summary.buttons = modules.iter().filter(|m| m.kind == ModuleKind::Button).count();
    summary.sequences = config.saved_sequences.len();
    Ok((config, summary))
}

const TRIGGER_KEYS: [&str; 3] = ["click", "doubleClick", "longPress"];

/// 读成配置之前，先看原文：动作类型都认得、参数都合法、「按住时」的值对、方案名合规、方案里的按键在布局里
fn check_raw(raw: &Value) -> Result<(), String> {
    let layout = raw
        .get("layout")
        .and_then(Value::as_object)
        .ok_or("文件里没有布局（layout），不像是蒜鸟导出的备份或者 config.json")?;
    let pages = layout.get("pages").and_then(Value::as_array).filter(|p| !p.is_empty()).ok_or("布局里一页都没有（layout.pages 是空的）")?;
    let mut buttons = HashSet::new();
    // 按键 id → 键上的字（出错时说「按键「删除」」比说 id 好懂）
    let mut labels = std::collections::HashMap::new();
    for page in pages {
        let page_name = page.get("name").and_then(Value::as_str).unwrap_or("?");
        for module in page.get("modules").and_then(Value::as_array).ok_or_else(|| format!("「{page_name}」页的模块列表不对"))? {
            let id = module.get("id").and_then(Value::as_str).ok_or_else(|| format!("「{page_name}」页有模块没有 id"))?;
            let place = format!("按键「{}」", module.get("label").and_then(Value::as_str).filter(|l| !l.is_empty()).unwrap_or(id));
            check_hold(module.get("hold"), &place)?;
            if let Some(behaviors) = module.get("behaviors") {
                check_triggers(behaviors, &place)?;
            }
            if module.get("kind").and_then(Value::as_str) == Some("button") {
                buttons.insert(id.to_string());
                labels.insert(id.to_string(), place.clone());
            }
        }
    }
    if let Some(profiles) = raw.get("profiles") {
        let profiles = profiles.as_array().ok_or("方案列表不对")?;
        let mut ids = HashSet::new();
        for profile in profiles {
            let name = profile.get("name").and_then(Value::as_str).unwrap_or("");
            if clean_profile_name(name).as_deref() != Some(name.trim()) || name.chars().count() > crate::profiles::MAX_PROFILE_NAME {
                return Err(format!("方案名不对：「{name}」（要 1～40 个字，不能有控制字符）"));
            }
            let id = profile.get("id").and_then(Value::as_str).filter(|i| !i.trim().is_empty()).ok_or_else(|| format!("方案「{name}」没有 id"))?;
            if !ids.insert(id.to_string()) {
                return Err(format!("方案 id 重复：{id}"));
            }
            let mapped = profile.get("buttons").and_then(Value::as_object).cloned().unwrap_or_default();
            for (button, mapping) in &mapped {
                if !buttons.contains(button) {
                    return Err(format!("方案「{name}」里有布局里没有的按键：{button}"));
                }
                let place = format!("方案「{name}」· {}", labels.get(button).cloned().unwrap_or_else(|| button.clone()));
                check_hold(mapping.get("hold"), &place)?;
                check_triggers(mapping, &place)?;
            }
        }
        if !profiles.is_empty() {
            let active = raw.get("activeProfileId").and_then(Value::as_str).unwrap_or("");
            if !ids.contains(active) {
                return Err("当前方案不在方案列表里".into());
            }
        }
    }
    if let Some(saved) = raw.get("savedSequences") {
        let mut ids = HashSet::new();
        for item in saved.as_array().ok_or("已保存的序列列表不对")? {
            let name = item.get("name").and_then(Value::as_str).unwrap_or("");
            if clean_profile_name(name).is_none() || name.chars().count() > crate::profiles::MAX_PROFILE_NAME {
                return Err(format!("已保存的序列名字不对：「{name}」"));
            }
            let id = item.get("id").and_then(Value::as_str).filter(|i| !i.trim().is_empty()).ok_or_else(|| format!("已保存的序列「{name}」没有 id"))?;
            if !ids.insert(id.to_string()) {
                return Err(format!("已保存的序列 id 重复：{id}"));
            }
            let steps = item.get("steps").and_then(Value::as_array).filter(|s| !s.is_empty()).ok_or_else(|| format!("已保存的序列「{name}」是空的"))?;
            check_steps(steps, &format!("已保存的序列「{name}」"))?;
        }
    }
    Ok(())
}

fn check_hold(hold: Option<&Value>, place: &str) -> Result<(), String> {
    match hold {
        None | Some(Value::Null) => Ok(()),
        Some(value) => match serde_json::from_value::<HoldMode>(value.clone()) {
            Ok(_) => Ok(()),
            Err(_) => Err(format!("{place} 的「按住时」不对：{value}")),
        },
    }
}

fn check_triggers(value: &Value, place: &str) -> Result<(), String> {
    let object = value.as_object().ok_or_else(|| format!("{place} 的动作格式不对"))?;
    for (key, list) in object {
        if key == "hold" {
            continue;
        }
        if !TRIGGER_KEYS.contains(&key.as_str()) {
            return Err(format!("{place} 有不支持的按法：{key}"));
        }
        let steps = list.as_array().ok_or_else(|| format!("{place} 的动作列表不对"))?;
        let trigger = Trigger::parse(key).map(Trigger::name).unwrap_or("");
        check_steps(steps, &format!("{place} · {trigger}"))?;
    }
    Ok(())
}

/// 每一步都要认得、参数合法
fn check_steps(steps: &[Value], place: &str) -> Result<(), String> {
    for (i, raw) in steps.iter().enumerate() {
        let step = Step::from_value(raw.clone());
        let at = format!("{place} · 第 {} 步", i + 1);
        match &step.action {
            Action::Unknown(raw) => {
                let kind = raw.get("type").and_then(Value::as_str).unwrap_or("?");
                return Err(format!("{at}：不认识或者不完整的动作（{kind}）"));
            }
            // 没选按键的一步（界面上刚加、还没选）不算错
            Action::Key { key } if !key.is_empty() && key_code(key).is_none() => return Err(format!("{at}：不认识的按键名「{key}」")),
            Action::Shortcut { keys } => {
                if let Some(bad) = keys.iter().find(|k| key_code(k).is_none()) {
                    return Err(format!("{at}：不认识的按键名「{bad}」"));
                }
            }
            Action::OpenWebsite { url } if normalize_url(url).is_none() => return Err(format!("{at}：网址不对「{url}」")),
            _ => {}
        }
    }
    Ok(())
}

/// 读成配置以后：布局完整（不重叠、不出界）、网格大小在界面允许的范围里、id 不重复
fn check_config(config: &Config) -> Result<(), String> {
    let layout = &config.layout;
    if !(2..=8).contains(&layout.columns) || !(3..=16).contains(&layout.rows) {
        return Err(format!("网格 {} 列 × {} 行超出范围（2～8 列、3～16 行）", layout.columns, layout.rows));
    }
    config.check_layout()?;
    let mut ids = HashSet::new();
    for page in &layout.pages {
        if page.id.trim().is_empty() || !ids.insert(page.id.clone()) {
            return Err(format!("页面 id 重复或者是空的：「{}」", page.name));
        }
        for module in &page.modules {
            if module.id.trim().is_empty() || !ids.insert(module.id.clone()) {
                return Err(format!("模块 id 重复或者是空的：「{}」", module.title()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_time() {
        assert_eq!(parse_iso_utc("2026-10-28T21:39:52Z"), Some(1_793_223_592));
        assert_eq!(parse_iso_utc(&iso_utc(1_700_000_000)), Some(1_700_000_000));
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(1_793_223_592), "2026-10-28T21:39:52Z"); // 北京时间 10-29 05:39:52
    }

    #[test]
    fn export_import_round_trip() {
        let mut config = Config::default();
        config.normalize();
        config.add_profile("工作", Some("default"));
        config.saved_sequences.push(crate::SavedSequence {
            id: "s1".into(),
            name: "删全部".into(),
            steps: vec![Step::new(Action::Shortcut { keys: vec!["Ctrl".into(), "A".into()] })],
            ..Default::default()
        });
        let text = export_document(&config, true, "0.5.0");
        let (back, summary) = parse_import(&text).unwrap();
        assert!(summary.backup);
        assert_eq!(summary.profiles, vec!["默认方案".to_string(), "工作".to_string()]);
        assert_eq!(summary.sequences, 1);
        assert_eq!(back.profiles, config.profiles);
        // 只导出配置：不含序列
        let (plain, summary) = parse_import(&export_document(&config, false, "0.5.0")).unwrap();
        assert_eq!((summary.backup, summary.sequences, plain.saved_sequences.len()), (false, 0, 0));
        // 直接是 config.json 也认
        assert!(parse_import(&crate::to_text(&config)).is_ok());
    }

    #[test]
    fn rejects_whole_file_on_one_bad_step() {
        let mut config = Config::default();
        config.normalize();
        let mut value = serde_json::to_value(&config).unwrap();
        value["profiles"][0]["buttons"]["enter"]["longPress"] = json!([{"type": "fooBar"}]);
        let err = parse_import(&value.to_string()).unwrap_err();
        assert!(err.contains("默认方案") && err.contains("长按") && err.contains("fooBar"), "{err}");
        let mut value = serde_json::to_value(&config).unwrap();
        value["layout"]["pages"][0]["modules"][1]["behaviors"]["click"] = json!([{"type": "key", "key": "Hyper"}]);
        assert!(parse_import(&value.to_string()).unwrap_err().contains("Hyper"));
        let mut value = serde_json::to_value(&config).unwrap();
        value["profiles"][0]["name"] = json!("");
        assert!(parse_import(&value.to_string()).is_err());
        let mut value = serde_json::to_value(&config).unwrap();
        value["layout"]["pages"][0]["modules"][1]["x"] = json!(0);
        value["layout"]["pages"][0]["modules"][1]["y"] = json!(0);
        assert!(parse_import(&value.to_string()).unwrap_err().contains("重叠"));
        let err = parse_import(r#"{"format":"axonkey-mapping","version":1}"#).unwrap_err();
        assert!(err.contains("遥控器映射") && !err.contains("axonkey"), "{err}");
    }

    #[test]
    fn reads_files_exported_before_rename() {
        let mut config = Config::default();
        config.normalize();
        let text = export_document(&config, true, "0.6.0").replace(BACKUP_FORMAT, "phonepad-backup");
        assert!(parse_import(&text).unwrap().1.backup);
        let text = export_document(&config, false, "0.6.0").replace(CONFIG_FORMAT, "phonepad-config");
        assert!(!parse_import(&text).unwrap().1.backup);
    }
}
