//! 多套方案：布局所有方案共用，每套方案只存每个按键的动作和「按住时」（和 axonkey 的配置方案对应）。
//!
//! 文件里是 `profiles: [{id, name, buttons: {模块 id: {hold, click, doubleClick, longPress}}}]` 加 `activeProfileId`。
//! 当前方案的映射同时照旧写在布局的模块上（`behaviors`、`hold`、`repeat`）：
//! - 后台、主界面、手机都照旧读模块上的，切方案就是把另一套抄到模块上；
//! - 旧版（0.4.0 及以前）读新配置照样能用当前方案，看不懂的 `profiles` 原样留着（Config 的 extra），不会丢；
//! - 两边对不上（旧版改了模块上的映射）以模块上的为准，写回当前方案。
//! 没有方案的旧配置读进来时，把现在的映射做成一个「默认方案」（以前叫「默认配置」，读到旧名字时改名）。

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{summarize, Action, Behaviors, Config, HoldMode, Module, ModuleKind, Step, Trigger};

/// 方案名最长多少个字（和 axonkey 一样）
pub const MAX_PROFILE_NAME: usize = 40;
pub const DEFAULT_PROFILE_ID: &str = "default";
pub const DEFAULT_PROFILE_NAME: &str = "默认方案";
/// 0.5.0 及以前默认方案的名字
const OLD_DEFAULT_PROFILE_NAME: &str = "默认配置";
/// 从遥控器映射导入成新方案时的名字
pub const IMPORTED_PROFILE_NAME: &str = "遥控器映射";

/// 一套方案
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Profile {
    pub id: String,
    pub name: String,
    /// 每个按键模块（模块 id）在这套方案里的映射；没有的就是什么都没配
    pub buttons: BTreeMap<String, ButtonMapping>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// 一个按键在某套方案里的映射
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ButtonMapping {
    #[serde(deserialize_with = "crate::lenient_hold", skip_serializing_if = "Option::is_none")]
    pub hold: Option<HoldMode>,
    pub click: Vec<Step>,
    pub double_click: Vec<Step>,
    pub long_press: Vec<Step>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// 起了名字保存下来的一串动作，以后可以插入或替换到别的按法
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SavedSequence {
    pub id: String,
    pub name: String,
    pub steps: Vec<Step>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ButtonMapping {
    /// 模块上现在的映射
    pub fn of(module: &Module) -> Self {
        let b = &module.behaviors;
        Self {
            hold: module.hold,
            click: b.click.clone(),
            double_click: b.double_click.clone(),
            long_press: b.long_press.clone(),
            extra: b.extra.clone(),
        }
    }

    /// 抄到模块上（repeat 跟着「按住时」，旧版也读得懂连发）
    pub fn apply(&self, module: &mut Module) {
        module.hold = self.hold;
        module.repeat = self.hold == Some(HoldMode::RepeatClick);
        module.behaviors = Behaviors {
            click: self.click.clone(),
            double_click: self.double_click.clone(),
            long_press: self.long_press.clone(),
            extra: self.extra.clone(),
        };
    }

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

/// 方案名：去掉两头空白和控制字符，最多 40 个字；剩下空的返回 None
pub fn clean_profile_name(name: &str) -> Option<String> {
    let cleaned: String = name.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(MAX_PROFILE_NAME).collect();
    let cleaned = cleaned.trim().to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// 「切换方案」的方向：（值，显示名）
pub const PROFILE_DIRECTIONS: [(&str, &str); 2] = [("next", "切到下一个方案"), ("previous", "切到上一个方案")];

impl Config {
    fn button_ids(&self) -> Vec<String> {
        self.layout.pages.iter().flat_map(|p| p.modules.iter()).filter(|m| m.kind == ModuleKind::Button).map(|m| m.id.clone()).collect()
    }

    fn button_modules_mut(&mut self) -> impl Iterator<Item = &mut Module> {
        self.layout.pages.iter_mut().flat_map(|p| p.modules.iter_mut()).filter(|m| m.kind == ModuleKind::Button)
    }

    /// 当前方案在列表里的位置；没有规整过（还没有方案、当前方案不在列表里）是 None
    fn active_index(&self) -> Option<usize> {
        self.profiles.iter().position(|p| p.id == self.active_profile_id)
    }

    /// 读进来以后规整方案（Config::normalize 里调）：
    /// 没有方案就把现在的映射做成「默认方案」；id、名字不合规的修好；当前方案不在了就用第一个；
    /// 模块上的映射写回当前方案（模块上的为准）；布局里已经没有的按键，各方案里的映射也删掉。
    /// rename_default：旧版的文件，「默认配置」改名「默认方案」
    pub(crate) fn normalize_profiles(&mut self, rename_default: bool) {
        if self.profiles.is_empty() {
            self.profiles.push(Profile { id: DEFAULT_PROFILE_ID.into(), name: DEFAULT_PROFILE_NAME.into(), ..Default::default() });
            self.active_profile_id = DEFAULT_PROFILE_ID.into();
        }
        if rename_default {
            for i in 0..self.profiles.len() {
                if self.profiles[i].name.trim() == OLD_DEFAULT_PROFILE_NAME {
                    let name = self.unique_profile_name(DEFAULT_PROFILE_NAME);
                    self.profiles[i].name = name;
                }
            }
        }
        let mut ids = HashSet::new();
        for i in 0..self.profiles.len() {
            let mut id = self.profiles[i].id.trim().to_string();
            if id.is_empty() || ids.contains(&id) {
                id = (1..).map(|n| format!("profile-{n}")).find(|c| !ids.contains(c) && !self.profiles.iter().any(|p| &p.id == c)).unwrap();
            }
            ids.insert(id.clone());
            let profile = &mut self.profiles[i];
            profile.id = id;
            profile.name = clean_profile_name(&profile.name).unwrap_or_else(|| format!("方案 {}", i + 1));
        }
        if !self.profiles.iter().any(|p| p.id == self.active_profile_id) {
            self.active_profile_id = self.profiles[0].id.clone();
        }
        let buttons: HashSet<String> = self.button_ids().into_iter().collect();
        self.sync_active_profile();
        for profile in &mut self.profiles {
            profile.buttons.retain(|id, _| buttons.contains(id));
            for (module_id, mapping) in profile.buttons.iter_mut() {
                for trigger in Trigger::ALL {
                    crate::number_steps(module_id, trigger, mapping.get_mut(trigger));
                }
            }
        }
    }

    /// 当前方案（规整过的配置一定有；没规整过的退回第一个）
    pub fn active_profile(&self) -> Option<&Profile> {
        self.active_index().and_then(|i| self.profiles.get(i)).or_else(|| self.profiles.first())
    }

    pub fn active_profile_name(&self) -> String {
        self.active_profile().map(|p| p.name.clone()).unwrap_or_else(|| DEFAULT_PROFILE_NAME.into())
    }

    /// 模块上的映射存回当前方案（当前方案不在列表里就不存：不能存进别的方案）
    pub fn sync_active_profile(&mut self) {
        let Some(index) = self.active_index() else { return };
        let mappings: Vec<(String, ButtonMapping)> = self
            .layout
            .pages
            .iter()
            .flat_map(|p| p.modules.iter())
            .filter(|m| m.kind == ModuleKind::Button)
            .map(|m| (m.id.clone(), ButtonMapping::of(m)))
            .collect();
        let profile = &mut self.profiles[index];
        for (id, mapping) in mappings {
            profile.buttons.insert(id, mapping);
        }
    }

    /// 把当前方案的映射抄到模块上（方案里没有的按键清空）
    fn load_active_profile(&mut self) {
        let Some(profile) = self.active_profile().cloned() else { return };
        for module in self.button_modules_mut() {
            profile.buttons.get(&module.id).cloned().unwrap_or_default().apply(module);
        }
    }

    /// 换到 id 这个方案：先把模块上的存回当前方案，再把它的抄到模块上。返回（原来的名字，新的名字）；已经是它、或者没有这个方案返回 None
    pub fn switch_profile(&mut self, id: &str) -> Option<(String, String)> {
        if id == self.active_profile_id || !self.profiles.iter().any(|p| p.id == id) {
            return None;
        }
        self.sync_active_profile();
        let from = self.active_profile_name();
        self.active_profile_id = id.to_string();
        self.load_active_profile();
        Some((from, self.active_profile_name()))
    }

    /// 「切换方案」要切到哪个方案（和遥控器映射软件一样）：只有一个方案、或者到头了又不回绕时返回 None
    pub fn adjacent_profile(&self, direction: &str, wrap: bool) -> Option<String> {
        let count = self.profiles.len() as i64;
        let index = self.profiles.iter().position(|p| p.id == self.active_profile_id)? as i64;
        if count < 2 {
            return None;
        }
        let mut next = index + if direction == "previous" { -1 } else { 1 };
        if next < 0 || next >= count {
            if !wrap {
                return None;
            }
            next = next.rem_euclid(count);
        }
        Some(self.profiles[next as usize].id.clone())
    }

    /// 不重复的方案名：base 已经有了就加「 2」「 3」……
    pub fn unique_profile_name(&self, base: &str) -> String {
        let base = clean_profile_name(base).unwrap_or_else(|| "新方案".into());
        let taken = |name: &str| self.profiles.iter().any(|p| p.name == name);
        if !taken(&base) {
            return base;
        }
        (2..)
            .map(|n| {
                let suffix = format!(" {n}");
                let head: String = base.chars().take(MAX_PROFILE_NAME - suffix.chars().count()).collect();
                format!("{head}{suffix}")
            })
            .find(|name| !taken(name))
            .unwrap()
    }

    /// 新建方案，返回新 id（不切过去）。copy_from：复制那个方案的映射；None：空方案（所有按键都没配）
    pub fn add_profile(&mut self, name: &str, copy_from: Option<&str>) -> String {
        self.sync_active_profile();
        let id = (1..).map(|n| format!("profile-{n}")).find(|c| !self.profiles.iter().any(|p| &p.id == c)).unwrap();
        let buttons = copy_from.and_then(|from| self.profiles.iter().find(|p| p.id == from)).map(|p| p.buttons.clone()).unwrap_or_default();
        let name = self.unique_profile_name(name);
        self.profiles.push(Profile { id: id.clone(), name, buttons, extra: Map::new() });
        id
    }

    pub fn rename_profile(&mut self, id: &str, name: &str) -> Result<String, String> {
        let name = clean_profile_name(name).ok_or("方案名不能是空的")?;
        if self.profiles.iter().any(|p| p.id != id && p.name == name) {
            return Err(format!("已经有叫「{name}」的方案了"));
        }
        let profile = self.profiles.iter_mut().find(|p| p.id == id).ok_or("没有这个方案")?;
        profile.name = name.clone();
        Ok(name)
    }

    /// 删除方案（至少留一个）。删的是当前方案就切到它后面那个（没有就前面那个）
    pub fn delete_profile(&mut self, id: &str) -> Result<(), String> {
        if self.profiles.len() <= 1 {
            return Err("至少要留一个方案".into());
        }
        let index = self.profiles.iter().position(|p| p.id == id).ok_or("没有这个方案")?;
        if id == self.active_profile_id {
            let next = if index + 1 < self.profiles.len() { index + 1 } else { index - 1 };
            // 删掉的方案不用再存
            self.active_profile_id = self.profiles[next].id.clone();
            self.profiles.remove(index);
            self.load_active_profile();
        } else {
            self.profiles.remove(index);
        }
        Ok(())
    }

    /// 每套方案里这个按键这个按法现在是什么（「切换方案」的确认框里列出来）：（名字，是不是当前方案，动作摘要）
    pub fn profile_sequences(&self, module_id: &str, trigger: Trigger) -> Vec<(String, bool, String)> {
        let mut config = self.clone();
        config.sync_active_profile();
        config
            .profiles
            .iter()
            .map(|p| {
                let steps = p.buttons.get(module_id).map(|b| b.get(trigger).clone()).unwrap_or_default();
                (p.name.clone(), p.id == config.active_profile_id, summarize(&steps))
            })
            .collect()
    }

    /// 设置「切换方案」：所有方案里这个按键的这个按法都换成只有这一步（保证切过去还能切回来）
    pub fn apply_shared_switch(&mut self, module_id: &str, trigger: Trigger, action: &Action) {
        self.sync_active_profile();
        for profile in &mut self.profiles {
            let mapping = profile.buttons.entry(module_id.to_string()).or_default();
            *mapping.get_mut(trigger) = vec![Step::new(action.clone())];
            crate::number_steps(module_id, trigger, mapping.get_mut(trigger));
        }
        self.load_active_profile();
    }

    /// 改「切换方案」的方向、回不回绕：所有方案里这个按键这个按法的「切换方案」跟着改
    pub fn update_shared_switch(&mut self, module_id: &str, trigger: Trigger, action: &Action) {
        self.sync_active_profile();
        for profile in &mut self.profiles {
            if let Some(mapping) = profile.buttons.get_mut(module_id) {
                for step in mapping.get_mut(trigger).iter_mut() {
                    if matches!(step.action, Action::SwitchProfile { .. }) {
                        step.action = action.clone();
                    }
                }
            }
        }
        self.load_active_profile();
    }

    /// 删除「切换方案」：所有方案里这个按键这个按法里的「切换方案」都删掉
    pub fn remove_shared_switch(&mut self, module_id: &str, trigger: Trigger) {
        self.sync_active_profile();
        for profile in &mut self.profiles {
            if let Some(mapping) = profile.buttons.get_mut(module_id) {
                mapping.get_mut(trigger).retain(|s| !matches!(s.action, Action::SwitchProfile { .. }));
            }
        }
        self.load_active_profile();
    }

    /// 别的方案里这个按键这个按法有「切换方案」的：方案名
    pub fn profiles_with_switch(&self, module_id: &str, trigger: Trigger) -> Vec<String> {
        let mut config = self.clone();
        config.sync_active_profile();
        config
            .profiles
            .iter()
            .filter(|p| p.buttons.get(module_id).is_some_and(|b| b.get(trigger).iter().any(|s| matches!(s.action, Action::SwitchProfile { .. }))))
            .map(|p| p.name.clone())
            .collect()
    }

    /// 新加了一页按键（导入遥控器映射），这些按键的映射放进一套新方案：新方案先复制当前方案，
    /// 再加上新按键的映射；当前方案里这些新按键先空着。切到新方案，返回（原来的名字，新方案的名字）
    pub fn add_page_as_profile(&mut self, mut page: crate::Page, name: &str) -> (String, String) {
        self.sync_active_profile();
        let current = self.active_profile_id.clone();
        let id = self.add_profile(name, Some(&current));
        let mappings: Vec<(String, ButtonMapping)> =
            page.modules.iter().filter(|m| m.kind == ModuleKind::Button).map(|m| (m.id.clone(), ButtonMapping::of(m))).collect();
        for module in page.modules.iter_mut().filter(|m| m.kind == ModuleKind::Button) {
            ButtonMapping::default().apply(module);
        }
        self.layout.pages.push(page);
        if let Some(profile) = self.profiles.iter_mut().find(|p| p.id == id) {
            profile.buttons.extend(mappings);
        }
        self.sync_active_profile();
        self.switch_profile(&id).unwrap_or_default()
    }

    /// 新的已保存序列 id
    pub fn new_sequence_id(&self) -> String {
        (1..).map(|n| format!("sequence-{n}")).find(|c| !self.saved_sequences.iter().any(|s| &s.id == c)).unwrap()
    }

    /// 删掉这些模块会丢掉多少映射：（配了动作的按键有几个，涉及几个方案）。删页面、删模块前的确认框里说
    pub fn mapping_impact(&self, module_ids: &[String]) -> (usize, usize) {
        let mut config = self.clone();
        config.sync_active_profile();
        let configured = |m: &ButtonMapping| Trigger::ALL.iter().any(|t| !m.get(*t).is_empty());
        let buttons = module_ids
            .iter()
            .filter(|id| config.profiles.iter().any(|p| p.buttons.get(*id).is_some_and(configured)))
            .count();
        let profiles = config.profiles.iter().filter(|p| module_ids.iter().any(|id| p.buttons.get(id).is_some_and(configured))).count();
        (buttons, profiles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse, to_text};

    fn key(k: &str) -> Action {
        Action::Key { key: k.into() }
    }

    #[test]
    fn migrates_and_switches() {
        let mut config = Config::default();
        config.normalize();
        assert_eq!(config.profiles.len(), 1);
        assert_eq!(config.active_profile_name(), DEFAULT_PROFILE_NAME);
        let enter = config.active_profile().unwrap().buttons.get("enter").unwrap().click.clone();
        assert_eq!(enter[0].action, key("Enter"));
        // 空方案：切过去所有按键都没配，切回来原样
        let empty = config.add_profile("空的", None);
        let copy = config.add_profile("默认方案", Some(DEFAULT_PROFILE_ID));
        assert_eq!(config.profiles.iter().find(|p| p.id == copy).unwrap().name, "默认方案 2");
        assert_eq!(config.switch_profile(&empty), Some(("默认方案".into(), "空的".into())));
        assert!(config.find_module("enter").unwrap().behaviors.click.is_empty());
        // 在空方案里改一下，切走再切回来还在
        config.layout.pages[0].modules.iter_mut().find(|m| m.id == "enter").unwrap().behaviors.click.push(Step::new(key("F5")));
        config.switch_profile(DEFAULT_PROFILE_ID);
        assert_eq!(config.find_module("enter").unwrap().behaviors.click[0].action, key("Enter"));
        config.switch_profile(&empty);
        assert_eq!(config.find_module("enter").unwrap().behaviors.click[0].action, key("F5"));
        // 存成文件再读回来一样
        let again = parse(&to_text(&config)).unwrap();
        assert_eq!(again.profiles, config.profiles);
        assert_eq!(again.active_profile_id, empty);
        // 相邻：回绕和到头停
        assert_eq!(config.adjacent_profile("next", true).as_deref(), Some(copy.as_str()));
        config.switch_profile(&copy);
        assert_eq!(config.adjacent_profile("next", false), None);
        assert_eq!(config.adjacent_profile("next", true).as_deref(), Some(DEFAULT_PROFILE_ID));
        assert_eq!(config.adjacent_profile("previous", false).as_deref(), Some(empty.as_str()));
        // 删当前方案：切到后面的（没有就前面的）；至少留一个
        config.delete_profile(&copy).unwrap();
        assert_eq!(config.active_profile_id, empty);
        config.delete_profile(DEFAULT_PROFILE_ID).unwrap();
        assert!(config.delete_profile(&empty).is_err());
    }

    /// 旧版改了模块上的映射（不认识 profiles）：读进来以模块上的为准
    #[test]
    fn module_mirror_wins() {
        let mut config = Config::default();
        config.normalize();
        let mut value = serde_json::to_value(&config).unwrap();
        let modules = value.pointer_mut("/layout/pages/0/modules").unwrap().as_array_mut().unwrap();
        let enter = modules.iter_mut().find(|m| m["id"] == "enter").unwrap();
        enter["behaviors"]["click"] = serde_json::json!([{"type": "key", "key": "Tab"}]);
        let config = parse(&value.to_string()).unwrap();
        assert_eq!(config.active_profile().unwrap().buttons["enter"].click[0].action, key("Tab"));
    }

    /// 第一次用（没有配置文件）：有「默认方案」；新增的空方案真的是空的，默认方案的映射不跟过去
    #[test]
    fn fresh_config_and_empty_profile() {
        let mut config = Config::fresh();
        assert_eq!(config.profiles.len(), 1);
        assert_eq!(config.active_profile_name(), DEFAULT_PROFILE_NAME);
        let empty = config.add_profile("新方案", None);
        config.switch_profile(&empty);
        assert!(config.find_module("esc").unwrap().behaviors.click.is_empty());
        assert_eq!(config.profiles.len(), 2);
        assert_eq!(config.profiles[0].buttons["esc"].click[0].action, key("Esc"));
        // 没规整过的配置：不会把模块上的映射存进新方案
        let mut raw = Config::default();
        let id = raw.add_profile("新方案", None);
        raw.switch_profile(&id);
        assert!(raw.profiles.iter().find(|p| p.id == id).unwrap().buttons.is_empty());
    }

    /// 旧版的「默认配置」读进来改名「默认方案」，其他不动；新版文件里叫「默认配置」的不改
    #[test]
    fn renames_old_default() {
        let mut config = Config::fresh();
        config.profiles[0].name = "默认配置".into();
        config.add_profile("工作", None);
        let mut value = serde_json::to_value(&config).unwrap();
        value["version"] = serde_json::json!(3);
        let old = parse(&value.to_string()).unwrap();
        assert_eq!(old.profiles[0].name, "默认方案");
        assert_eq!(old.profiles[1].name, "工作");
        assert_eq!(old.version, crate::CONFIG_VERSION);
        assert_eq!(old.profiles[0].buttons, config.profiles[0].buttons);
        value["version"] = serde_json::json!(4);
        assert_eq!(parse(&value.to_string()).unwrap().profiles[0].name, "默认配置");
    }

    #[test]
    fn shared_switch() {
        let mut config = Config::default();
        config.normalize();
        let other = config.add_profile("工作", None);
        let switch = Action::SwitchProfile { direction: "next".into(), wrap: true };
        config.apply_shared_switch("esc", Trigger::LongPress, &switch);
        for profile in &config.profiles {
            assert_eq!(profile.buttons["esc"].long_press.len(), 1);
        }
        assert_eq!(config.find_module("esc").unwrap().behaviors.long_press[0].action, switch);
        assert_eq!(config.profiles_with_switch("esc", Trigger::LongPress).len(), 2);
        config.switch_profile(&other);
        assert_eq!(config.find_module("esc").unwrap().behaviors.long_press[0].action, switch);
        config.remove_shared_switch("esc", Trigger::LongPress);
        assert!(config.profiles.iter().all(|p| p.buttons["esc"].long_press.is_empty()));
    }
}
