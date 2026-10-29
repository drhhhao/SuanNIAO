//! 一键预设：常用的、写代码和终端常用的（vibe coding）、导航和鼠标、媒体四组，最后单独一组「清空」。
//! 选了就把当前按法的动作整个换掉（可以撤销）。

use crate::actions::{Action, DEFAULT_CURSOR_DISTANCE};

pub struct Preset {
    pub group: &'static str,
    pub name: &'static str,
    pub actions: Vec<Action>,
}

fn key(k: &str) -> Vec<Action> {
    vec![Action::Key { key: k.into() }]
}

fn keys(k: &[&str]) -> Action {
    Action::Shortcut { keys: k.iter().map(|s| s.to_string()).collect() }
}

pub fn presets() -> Vec<Preset> {
    let wheel = |d: &str| vec![Action::Wheel { direction: d.into() }];
    let mouse = |b: &str| vec![Action::Mouse { button: b.into() }];
    let cursor = |d: &str| vec![Action::CursorMove { direction: d.into(), distance: DEFAULT_CURSOR_DISTANCE }];
    let p = |group, name, actions| Preset { group, name, actions };
    vec![
        p("常用", "确认 / 回车（Enter）", key("Enter")),
        p("常用", "返回 / 取消（Esc）", key("Esc")),
        p("常用", "空格", key("Space")),
        p("常用", "切换焦点（Tab）", key("Tab")),
        p(
            "常用",
            "输入文本并回车（文字填在第一步）",
            vec![Action::Paste { text: String::new(), mode: None }, Action::Delay { ms: 30 }, Action::Key { key: "Enter".into() }],
        ),
        p("常用", "回到桌面（Win+D）", vec![keys(&["Win", "D"])]),
        p("常用", "上一标签页（Ctrl+Shift+Tab）", vec![keys(&["Ctrl", "Shift", "Tab"])]),
        p("常用", "下一标签页（Ctrl+Tab）", vec![keys(&["Ctrl", "Tab"])]),
        p("写代码和终端", "换行不发送（Shift+Enter）", vec![keys(&["Shift", "Enter"])]),
        p("写代码和终端", "中断 / 停止（Ctrl+C）", vec![keys(&["Ctrl", "C"])]),
        p("写代码和终端", "清屏（Ctrl+L）", vec![keys(&["Ctrl", "L"])]),
        p("写代码和终端", "撤销（Ctrl+Z）", vec![keys(&["Ctrl", "Z"])]),
        p("写代码和终端", "重做（Ctrl+Y）", vec![keys(&["Ctrl", "Y"])]),
        p("写代码和终端", "切换模式（Shift+Tab）", vec![keys(&["Shift", "Tab"])]),
        p("写代码和终端", "上一条命令（↑）", key("Up")),
        p("写代码和终端", "删前一个词（Ctrl+Backspace）", vec![keys(&["Ctrl", "Backspace"])]),
        p("写代码和终端", "删除全部文本（Ctrl+A，Backspace）", vec![keys(&["Ctrl", "A"]), Action::Delay { ms: 30 }, Action::Key { key: "Backspace".into() }]),
        p("写代码和终端", "复制（Ctrl+C）", vec![keys(&["Ctrl", "C"])]),
        p("写代码和终端", "粘贴（Ctrl+V）", vec![keys(&["Ctrl", "V"])]),
        p("写代码和终端", "保存（Ctrl+S）", vec![keys(&["Ctrl", "S"])]),
        p("写代码和终端", "命令面板（Ctrl+Shift+P）", vec![keys(&["Ctrl", "Shift", "P"])]),
        p("导航和鼠标", "方向上", key("Up")),
        p("导航和鼠标", "方向下", key("Down")),
        p("导航和鼠标", "方向左", key("Left")),
        p("导航和鼠标", "方向右", key("Right")),
        p("导航和鼠标", "向前删除（Backspace）", key("Backspace")),
        p("导航和鼠标", "向后删除（Delete）", key("Delete")),
        p("导航和鼠标", "跳到开头（Home）", key("Home")),
        p("导航和鼠标", "跳到结尾（End）", key("End")),
        p("导航和鼠标", "向上翻页（PageUp）", key("PageUp")),
        p("导航和鼠标", "向下翻页（PageDown）", key("PageDown")),
        p("导航和鼠标", "鼠标滚轮向上", wheel("up")),
        p("导航和鼠标", "鼠标滚轮向下", wheel("down")),
        p("导航和鼠标", "鼠标滚轮向左", wheel("left")),
        p("导航和鼠标", "鼠标滚轮向右", wheel("right")),
        p("导航和鼠标", "鼠标左键", mouse("left")),
        p("导航和鼠标", "鼠标右键", mouse("right")),
        p("导航和鼠标", "光标上移 50 像素", cursor("up")),
        p("导航和鼠标", "光标下移 50 像素", cursor("down")),
        p("导航和鼠标", "光标左移 50 像素", cursor("left")),
        p("导航和鼠标", "光标右移 50 像素", cursor("right")),
        p("导航和鼠标", "右 Ctrl", key("RCtrl")),
        p("导航和鼠标", "右 Alt", key("RAlt")),
        p("导航和鼠标", "右 Win", key("RWin")),
        p("媒体", "播放 / 暂停", key("MediaPlayPause")),
        p("媒体", "上一首", key("MediaPrevious")),
        p("媒体", "下一首", key("MediaNext")),
        p("媒体", "停止播放", key("MediaStop")),
        p("媒体", "增大音量", key("VolumeUp")),
        p("媒体", "减小音量", key("VolumeDown")),
        p("媒体", "静音", key("VolumeMute")),
        p("清空", "清空动作（按了没反应）", vec![]),
        p("清空", "禁用（放一步「禁用」，什么都不做）", vec![Action::Disabled]),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn presets_are_valid() {
        for preset in super::presets() {
            for action in &preset.actions {
                if let Some(keys) = action.chord() {
                    assert!(keys.iter().all(|k| crate::key_code(k).is_some()), "{}", preset.name);
                }
            }
        }
        let all = super::presets();
        assert_eq!(all.len(), 53);
        // 清空放在最后
        assert_eq!(all.last().unwrap().group, "清空");
        // 删除全部文本不动剪贴板
        let clear = all.iter().find(|p| p.name.starts_with("删除全部文本")).unwrap();
        assert!(!clear.actions.iter().any(|a| a.chord() == Some(vec!["Ctrl".into(), "X".into()])));
    }
}
