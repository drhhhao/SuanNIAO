//! 撤销、重做：布局、映射和设置页的编辑，最多 100 步。存的是改之前的整份配置和这一步做了什么（提示「已撤销：…」用）；
//! 暂停按键、开机自启不算（撤销时保留现在的）。
//!
//! - 连着打字、连着点加减按钮这类同一处的改动，1.5 秒内的合成一步；
//! - 配置文件被别处改了（手机上调了布局、托盘暂停、手机上切了方案）：把同样的改动三方合并进每一步，
//!   合并不了的那一步和更早的都丢掉，撤销时不会把别人的改动也一起撤掉、也不会弄乱配置。

use std::time::{Duration, Instant};

use phonepad_config::{self as config, Config};

const LIMIT: usize = 100;
const MERGE_WINDOW: Duration = Duration::from_millis(1500);

/// 一步：改之前的配置、做了什么
pub struct Entry {
    config: Config,
    label: String,
}

#[derive(Default)]
pub struct History {
    /// 从旧到新
    undo: Vec<Entry>,
    /// 最近撤销的在最后
    redo: Vec<Entry>,
    /// 上一次记录的是哪一处、什么时候（合并连着的改动）
    last: Option<(String, Instant)>,
}

/// 撤销不管暂停按键和开机自启：其余的（布局、方案、序列、设置）一样就算一样
fn same(a: &Config, b: &Config) -> bool {
    let strip = |c: &Config| {
        let mut c = c.clone();
        c.settings.enabled = true;
        c.settings.autostart = false;
        config::to_text(&c)
    };
    strip(a) == strip(b)
}

fn push_limited(stack: &mut Vec<Entry>, item: Entry) {
    stack.push(item);
    if stack.len() > LIMIT {
        stack.remove(0);
    }
}

impl History {
    /// 改之前调：记下改之前的样子和这一步做了什么。key 是改的哪一处（同一处 1.5 秒内的改动合成一步），None 是单独的一步
    pub fn record(&mut self, before: &Config, key: Option<&str>, label: &str) {
        let now = Instant::now();
        if let (Some(key), Some((last, at))) = (key, &self.last) {
            if key == last && now.duration_since(*at) < MERGE_WINDOW {
                self.last = Some((key.to_string(), now));
                self.redo.clear();
                return;
            }
        }
        self.last = key.map(|k| (k.to_string(), now));
        self.redo.clear();
        if let Some(top) = self.undo.last_mut() {
            if same(&top.config, before) {
                top.label = label.to_string();
                return;
            }
        }
        push_limited(&mut self.undo, Entry { config: before.clone(), label: label.to_string() });
    }

    /// 撤销：返回要换成的配置和撤销的是什么（和现在一样的步骤跳过）
    pub fn undo(&mut self, current: &Config) -> Option<(Config, String)> {
        self.last = None;
        while let Some(previous) = self.undo.pop() {
            if same(&previous.config, current) {
                continue;
            }
            push_limited(&mut self.redo, Entry { config: current.clone(), label: previous.label.clone() });
            return Some((previous.config, previous.label));
        }
        None
    }

    pub fn redo(&mut self, current: &Config) -> Option<(Config, String)> {
        self.last = None;
        while let Some(next) = self.redo.pop() {
            if same(&next.config, current) {
                continue;
            }
            push_limited(&mut self.undo, Entry { config: current.clone(), label: next.label.clone() });
            return Some((next.config, next.label));
        }
        None
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// 配置文件被别处从 base 改成了 theirs：每一步都合进同样的改动，合不了的和更早的丢掉
    pub fn rebase(&mut self, base: &Config, theirs: &Config) {
        self.last = None;
        rebase_stack(&mut self.undo, base, theirs);
        rebase_stack(&mut self.redo, base, theirs);
    }
}

/// 从离现在最近的一步往回合并；第 i 步合不了，就把它和更远的（下标更小的）都丢掉
fn rebase_stack(stack: &mut Vec<Entry>, base: &Config, theirs: &Config) {
    for i in (0..stack.len()).rev() {
        match config::merge_configs_strict(base, &stack[i].config, theirs) {
            Some(mut merged) => {
                merged.normalize();
                stack[i].config = merged;
            }
            None => {
                stack.drain(..=i);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_label(config: &Config, id: &str, label: &str) -> Config {
        let mut c = config.clone();
        c.layout.pages[0].modules.iter_mut().find(|m| m.id == id).unwrap().label = label.into();
        c
    }

    #[test]
    fn undo_redo_and_merge_typing() {
        let mut base = Config::default();
        base.normalize();
        let mut h = History::default();
        let a = with_label(&base, "enter", "A");
        h.record(&base, Some("label:enter"), "改「回车」的名字");
        h.record(&a, Some("label:enter"), "改「回车」的名字"); // 连着打字：合成一步
        let ab = with_label(&base, "enter", "AB");
        h.record(&ab, None, "改「Esc」的名字");
        let abc = with_label(&ab, "esc", "X");
        let (undone, label) = h.undo(&abc).unwrap();
        assert_eq!(undone.find_module("esc").unwrap().label, ab.find_module("esc").unwrap().label);
        assert_eq!(label, "改「Esc」的名字");
        let (back, label) = h.undo(&ab).unwrap();
        assert_eq!(back.find_module("enter").unwrap().label, base.find_module("enter").unwrap().label);
        assert_eq!(label, "改「回车」的名字");
        assert!(!h.can_undo());
        let (again, label) = h.redo(&back).unwrap();
        assert_eq!(again.find_module("enter").unwrap().label, "AB");
        assert_eq!(label, "改「回车」的名字");
        // 设置也能撤销，暂停不算
        let mut h = History::default();
        let mut slow = base.clone();
        slow.settings.touchpad.sensitivity = 1.0;
        h.record(&base, Some("settings:touchpad.sensitivity"), "改指针速度");
        let (back, _) = h.undo(&slow).unwrap();
        assert_eq!(back.settings.touchpad.sensitivity, base.settings.touchpad.sensitivity);
        let mut h = History::default();
        let mut paused = base.clone();
        paused.settings.enabled = false;
        h.record(&base, None, "暂停");
        assert!(h.undo(&paused).is_none());
    }

    #[test]
    fn external_change_is_kept() {
        let mut base = Config::default();
        base.normalize();
        let mut h = History::default();
        h.record(&base, None, "改名字");
        let mine = with_label(&base, "enter", "改过");
        // 别处改了另一个按键：撤销以后它还在
        let theirs = with_label(&mine, "esc", "手机改的");
        h.rebase(&mine, &theirs);
        let (undone, _) = h.undo(&theirs).unwrap();
        assert_eq!(undone.find_module("esc").unwrap().label, "手机改的");
        assert_eq!(undone.find_module("enter").unwrap().label, base.find_module("enter").unwrap().label);
        // 别处改了同一个地方：合不了，那一步丢掉
        let mut h = History::default();
        h.record(&base, None, "改名字");
        let clash = with_label(&mine, "enter", "手机也改了");
        h.rebase(&mine, &clash);
        assert!(!h.can_undo());
    }
}
