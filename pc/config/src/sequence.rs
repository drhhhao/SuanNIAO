//! 动作序列的文字格式（「复制动作」「粘贴动作」用）。和 axonkey 的「复制序列」是同一种写法，
//! 开头写 suanniao/1；粘贴时也认改名前的 phonepad/1，和开头是 axonkey/1 的（从遥控器映射软件复制来的），但界面上不提它：
//!
//! ```text
//! suanniao/1
//! key Enter
//! keys Ctrl Shift Tab
//! wait 30ms
//! text "你好"
//! wheel up
//! mouse left
//! move up 50
//! app "C:\\Program Files\\Foo\\foo.exe"
//! url https://example.com/
//! profile next wrap
//! off key Esc
//! end
//! ```
//!
//! 一行一步，`off` 开头是停用的步骤；参数里有空白、引号、反斜杠的写成 JSON 字符串。
//! 蒜鸟多出来的写在行尾，axonkey 读不懂也没关系：`text "…" type|clipboard`（粘贴方式）、
//! `app "…" args "…" activate`（启动参数、已经在运行就切过去）、`mouse middle`。
//! 粘贴时能从引用块（`>` 开头）、Markdown 代码块里认出来；格式不对时指出是第几行。

use std::fmt;

use crate::actions::{key_code, new_step_id, normalize_url, Action, PasteMode, Step, DIRECTIONS, MAX_CURSOR_DISTANCE, MAX_DELAY_MS, MOUSE_BUTTONS};

pub const HEADER: &str = "suanniao/1";
/// 改名前（PhonePad）复制出来的开头
const OLD_HEADER: &str = "phonepad/1";
/// 遥控器映射软件复制出来的开头：内容的写法一样，照样认
const LEGACY_HEADER: &str = "axonkey/1";

/// 像开头的一行：「名字/数字」（suanniao/1、phonepad/1、axonkey/1、以后的 suanniao/2）
fn is_header(line: &str) -> bool {
    let Some((name, version)) = line.split_once('/') else { return false };
    (name == "suanniao" || name == "phonepad" || name == "axonkey") && !version.is_empty() && version.chars().all(|c| c.is_ascii_digit())
}

/// 写成文字。看不懂的步骤（axonkey 也表示不了）和空的组合键跳过，返回（文字，跳过了几步）
pub fn serialize(steps: &[Step]) -> (String, usize) {
    let mut lines = vec![HEADER.to_string()];
    let mut skipped = 0;
    for step in steps {
        match line_of(&step.action) {
            Some(line) => lines.push(if step.enabled { line } else { format!("off {line}") }),
            None => skipped += 1,
        }
    }
    lines.push("end".into());
    (lines.join("\n"), skipped)
}

/// 参数：没有空白、引号、反斜杠的原样写，否则写成 JSON 字符串（和 axonkey 一样）
fn arg(value: &str) -> String {
    if !value.is_empty() && !value.chars().any(|c| c.is_whitespace() || c == '"' || c == '\\') {
        value.to_string()
    } else {
        serde_json::to_string(value).unwrap_or_default()
    }
}

fn line_of(action: &Action) -> Option<String> {
    Some(match action {
        Action::Key { key } if key.is_empty() => return None,
        Action::Key { key } => format!("key {}", arg(key)),
        Action::Shortcut { keys } if !keys.is_empty() => format!("keys {}", keys.iter().map(|k| arg(k)).collect::<Vec<_>>().join(" ")),
        Action::Shortcut { .. } => return None,
        Action::Delay { ms } => format!("wait {ms}ms"),
        Action::Paste { text, mode } => {
            let mut line = format!("text {}", serde_json::to_string(text).unwrap_or_default());
            match mode {
                Some(PasteMode::Type) => line.push_str(" type"),
                Some(PasteMode::Clipboard) => line.push_str(" clipboard"),
                None => {}
            }
            line
        }
        Action::Wheel { direction } => format!("wheel {direction}"),
        Action::Mouse { button } => format!("mouse {button}"),
        Action::CursorMove { direction, distance } => format!("move {direction} {distance}"),
        Action::OpenApp { path, args, activate } => {
            let mut line = format!("app {}", arg(path));
            if !args.is_empty() {
                line.push_str(&format!(" args {}", arg(args)));
            }
            if *activate {
                line.push_str(" activate");
            }
            line
        }
        Action::OpenWebsite { url } => format!("url {}", arg(url)),
        Action::SwitchProfile { direction, wrap } => format!("profile {direction} {}", if *wrap { "wrap" } else { "stop" }),
        Action::Disabled => "disabled".into(),
        Action::Unknown(_) => return None,
    })
}

/// 读不懂的地方：第几行（从 1 数，0 表示整段）、什么问题
#[derive(Debug, PartialEq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 { write!(f, "{}", self.message) } else { write!(f, "第 {} 行：{}", self.line, self.message) }
    }
}

/// 从文字读出一串动作，每步都给新的 id。有一行不对就整段不要
pub fn parse(text: &str) -> Result<Vec<Step>, ParseError> {
    let error = |line: usize, message: String| ParseError { line, message };
    // 引用块的 > 去掉；代码块的 ``` 在 axonkey/1 和 end 外面，不用管
    let lines: Vec<String> = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|line| {
            let mut line = line.trim();
            while let Some(rest) = line.strip_prefix('>') {
                line = rest.trim_start();
            }
            line.trim().to_string()
        })
        .collect();
    let headers: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| is_header(l)).map(|(i, _)| i).collect();
    let Some(&header) = headers.first() else {
        return Err(error(0, "剪贴板里不是动作：先在别的按法上点「复制动作」，再来粘贴".into()));
    };
    if headers.len() > 1 {
        return Err(error(headers[1] + 1, "里面有好几段动作，一次只能粘贴一段".into()));
    }
    if ![HEADER, OLD_HEADER, LEGACY_HEADER].contains(&lines[header].as_str()) {
        return Err(error(header + 1, "这是更新的版本复制的动作，这个版本看不懂，先升级蒜鸟".into()));
    }
    let end = lines[header + 1..]
        .iter()
        .position(|l| l == "end")
        .map(|p| p + header + 1)
        .ok_or_else(|| error(lines.len(), "没找到结尾的 end".into()))?;
    let mut steps = Vec::new();
    for (i, line) in lines.iter().enumerate().take(end).skip(header + 1) {
        if line.is_empty() {
            continue;
        }
        steps.push(parse_line(line).map_err(|message| error(i + 1, message))?);
    }
    Ok(steps)
}

/// 一行拆成记号：JSON 字符串，或者一串不带空白和引号的字；记号之间要有空白
fn tokens(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut rest = line.trim_start();
    while !rest.is_empty() {
        if rest.starts_with('"') {
            let bytes = rest.as_bytes();
            let mut i = 1;
            let mut closed = None;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => {
                        closed = Some(i);
                        break;
                    }
                    _ => i += 1,
                }
            }
            let end = closed.ok_or("引号没有配对")?;
            let token: String = serde_json::from_str(&rest[..=end]).map_err(|_| "引号里的转义写法不对".to_string())?;
            rest = &rest[end + 1..];
            if rest.chars().next().is_some_and(|c| !c.is_whitespace()) {
                return Err("引号后面要空一格".into());
            }
            out.push(token);
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let token = &rest[..end];
            if token.contains('"') {
                return Err("引号要放在参数开头".into());
            }
            out.push(token.to_string());
            rest = &rest[end..];
        }
        rest = rest.trim_start();
    }
    Ok(out)
}

fn parse_line(line: &str) -> Result<Step, String> {
    let mut tokens = tokens(line)?;
    let enabled = if tokens.first().map(String::as_str) == Some("off") {
        tokens.remove(0);
        false
    } else {
        true
    };
    if tokens.is_empty() {
        return Err("off 后面没有动作".into());
    }
    let command = tokens.remove(0);
    let args = tokens;
    let count = |n: usize| -> Result<(), String> {
        if args.len() == n { Ok(()) } else { Err(format!("「{command}」后面要 {n} 个参数，这里有 {} 个", args.len())) }
    };
    let check_key = |key: &str| -> Result<(), String> {
        if key_code(key).is_some() { Ok(()) } else { Err(format!("不认识的按键名「{key}」")) }
    };
    let direction = |value: &str| -> Result<String, String> {
        if DIRECTIONS.iter().any(|(code, _)| *code == value) { Ok(value.to_string()) } else { Err(format!("方向只能是 up、down、left、right，不能是「{value}」")) }
    };
    let action = match command.as_str() {
        "key" => {
            count(1)?;
            check_key(&args[0])?;
            Action::Key { key: args[0].clone() }
        }
        "keys" => {
            if args.is_empty() {
                return Err("keys 后面至少要一个按键".into());
            }
            for key in &args {
                check_key(key)?;
            }
            Action::Shortcut { keys: args.clone() }
        }
        "wait" => {
            count(1)?;
            let number = args[0].strip_suffix("ms").unwrap_or(&args[0]);
            let ms: f64 = number.parse().ok().filter(|v: &f64| v.is_finite() && *v >= 0.0).ok_or_else(|| format!("等待的时间写得不对：「{}」", args[0]))?;
            if ms > MAX_DELAY_MS as f64 {
                return Err(format!("等待最多 {MAX_DELAY_MS} 毫秒"));
            }
            Action::Delay { ms: ms.round() as u64 }
        }
        "text" => {
            let mode = match args.get(1).map(String::as_str) {
                None => None,
                Some("type") => Some(PasteMode::Type),
                Some("clipboard") => Some(PasteMode::Clipboard),
                Some(other) => return Err(format!("粘贴方式只能是 type 或 clipboard，不能是「{other}」")),
            };
            if args.is_empty() || args.len() > 2 {
                return Err("text 后面要一段文字（可以再写 type 或 clipboard）".into());
            }
            Action::Paste { text: args[0].clone(), mode }
        }
        "wheel" => {
            count(1)?;
            Action::Wheel { direction: direction(&args[0])? }
        }
        "mouse" => {
            count(1)?;
            if !MOUSE_BUTTONS.iter().any(|(code, _)| *code == args[0]) {
                return Err(format!("鼠标键只能是 left、right、middle、back、forward，不能是「{}」", args[0]));
            }
            Action::Mouse { button: args[0].clone() }
        }
        "move" => {
            count(2)?;
            let distance: u32 = args[1].parse().map_err(|_| format!("移动的距离要是整数：「{}」", args[1]))?;
            if !(1..=MAX_CURSOR_DISTANCE).contains(&distance) {
                return Err(format!("移动的距离要在 1～{MAX_CURSOR_DISTANCE} 之间"));
            }
            Action::CursorMove { direction: direction(&args[0])?, distance }
        }
        "app" => {
            let Some(path) = args.first() else { return Err("app 后面要写程序的路径".into()) };
            let (mut app_args, mut activate) = (String::new(), false);
            let mut rest = args[1..].iter();
            while let Some(word) = rest.next() {
                match word.as_str() {
                    "activate" => activate = true,
                    "args" => app_args = rest.next().ok_or("args 后面要写启动参数")?.clone(),
                    other => return Err(format!("app 后面不认识「{other}」（只认 args 和 activate）")),
                }
            }
            Action::OpenApp { path: path.trim().trim_matches('"').to_string(), args: app_args, activate }
        }
        "url" => {
            count(1)?;
            Action::OpenWebsite { url: normalize_url(&args[0]).ok_or_else(|| format!("网址不对：「{}」（只能是 http、https）", args[0]))? }
        }
        "profile" => {
            count(2)?;
            if args[0] != "previous" && args[0] != "next" {
                return Err(format!("方向只能是 previous 或 next，不能是「{}」", args[0]));
            }
            let wrap = match args[1].as_str() {
                "wrap" => true,
                "stop" => false,
                other => return Err(format!("到头后只能是 wrap 或 stop，不能是「{other}」")),
            };
            Action::SwitchProfile { direction: args[0].clone(), wrap }
        }
        "disabled" => {
            count(0)?;
            Action::Disabled
        }
        other => return Err(format!("不认识「{other}」这个动作")),
    };
    Ok(Step { id: new_step_id(), enabled, action })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_old_header() {
        // axonkey 复制出来的原文（behaviorModel.ts 的格式）
        let text = "这是我的序列：\n```\naxonkey/1\nkeys Ctrl A\nwait 30ms\nkeys Ctrl X\ntext \"多行\\n文本 \\\"引号\\\"\"\napp \"C:\\\\Program Files\\\\a b.exe\"\nurl https://example.com/\nprofile previous stop\noff disabled\nend\n```";
        let steps = parse(text).unwrap();
        assert_eq!(steps.len(), 8);
        assert_eq!(steps[0].action, Action::Shortcut { keys: vec!["Ctrl".into(), "A".into()] });
        assert_eq!(steps[1].action, Action::Delay { ms: 30 });
        assert_eq!(steps[3].action, Action::Paste { text: "多行\n文本 \"引号\"".into(), mode: None });
        assert_eq!(steps[4].action, Action::OpenApp { path: "C:\\Program Files\\a b.exe".into(), args: String::new(), activate: false });
        assert_eq!(steps[6].action, Action::SwitchProfile { direction: "previous".into(), wrap: false });
        assert!(!steps[7].enabled);
        // 写回去再读，一样
        let (again, skipped) = serialize(&steps);
        assert_eq!(skipped, 0);
        let back = parse(&again).unwrap();
        assert_eq!(back.iter().map(|s| (&s.action, s.enabled)).collect::<Vec<_>>(), steps.iter().map(|s| (&s.action, s.enabled)).collect::<Vec<_>>());
        // 蒜鸟多出来的写在行尾
        let extra = vec![
            Step::new(Action::Paste { text: "x".into(), mode: Some(PasteMode::Type) }),
            Step::new(Action::OpenApp { path: "C:\\a.exe".into(), args: "--x y".into(), activate: true }),
            Step::new(Action::Mouse { button: "middle".into() }),
        ];
        let (text, _) = serialize(&extra);
        assert!(text.contains("text \"x\" type") && text.contains("args \"--x y\" activate") && text.contains("mouse middle"), "{text}");
        assert_eq!(parse(&text).unwrap().iter().map(|s| s.action.clone()).collect::<Vec<_>>(), extra.iter().map(|s| s.action.clone()).collect::<Vec<_>>());
        // 引用块
        assert_eq!(parse("> axonkey/1\n> key Enter\n> end").unwrap().len(), 1);
        // 自己复制出来的开头是 suanniao/1；没选按键的一步不复制
        let (text, skipped) = serialize(&[Step::new(Action::Key { key: "Enter".into() }), Step::new(Action::Key { key: String::new() })]);
        assert!(text.starts_with("suanniao/1\n") && skipped == 1, "{text}");
        // 改名前复制的 phonepad/1 也认
        assert_eq!(parse("phonepad/1\nkey Enter\nend").unwrap().len(), 1);
        assert_eq!(parse(&text).unwrap().len(), 1);
    }

    #[test]
    fn errors_point_at_the_line() {
        assert_eq!(parse("axonkey/1\nkey Enter\nkey Foo\nend").unwrap_err().line, 3);
        assert_eq!(parse("axonkey/1\nmove up 900\nend").unwrap_err().line, 2);
        assert_eq!(parse("axonkey/1\ntext \"abc\nend").unwrap_err().line, 2);
        assert_eq!(parse("axonkey/1\nurl javascript:alert(1)\nend").unwrap_err().line, 2);
        assert_eq!(parse("suanniao/2\nend").unwrap_err().line, 1);
        assert!(!parse("suanniao/1\nkey Enter\nend\nsuanniao/1\nend").unwrap_err().message.contains("axonkey"));
        assert!(!parse("随便").unwrap_err().message.contains("axonkey"));
        assert_eq!(parse("axonkey/1\nkey Enter").unwrap_err().message, "没找到结尾的 end");
        assert_eq!(parse("随便一段话").unwrap_err().line, 0);
    }
}
