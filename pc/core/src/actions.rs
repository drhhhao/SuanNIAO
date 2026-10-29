//! 执行按键映射里的动作（类型和 axonkey 一致）。
//!
//! 动作放到一个专门的执行线程里按时间排队执行，这个线程初始化了 COM（单线程套间，打开应用、网站时 Shell 要用）：
//! - 「等待」只推迟同一串里后面的步骤，不卡收蓝牙消息的线程，也不耽误别的按键；
//! - 各步之间没有固定间隔（和 axonkey 一样）；
//! - 改配置、断线、暂停时 cancel() 取消还没执行的步骤；
//! - 连发产生的步骤过了时间还没轮到就跳过（stale），动作慢时不会越积越多，松手后也不会接着按；
//! - 「切换方案」换掉了所有按键的映射：和改配置一样，后面还没执行的步骤不做了、直通按着的键松开，
//!   然后推给手机、桌面上弹通知。

use std::sync::{Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};

use phonepad_config::{Action, PasteMode, Step};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

use crate::{input, launcher, link, log, overlay, state};

/// 一次提交的执行方式
#[derive(Clone, Copy, Debug)]
pub struct RunOptions {
    /// 粘贴文本没指定方式时用哪种（设置里的「文字上屏方式」）：true = 剪贴板
    pub paste_by_clipboard: bool,
    /// 连发产生的：到时间后这么久还没轮到就跳过
    pub stale_after: Option<Duration>,
    /// 光标移动的倍数（连发时的加速度）
    pub cursor_scale: f64,
}

impl RunOptions {
    pub fn new(paste_by_clipboard: bool) -> Self {
        Self { paste_by_clipboard, stale_after: None, cursor_scale: 1.0 }
    }
}

struct Job {
    due: Instant,
    order: u64,
    action: Action,
    options: RunOptions,
}

#[derive(Default)]
struct Queue {
    jobs: Vec<Job>,
    next_order: u64,
    started: bool,
}

/// 一次最多排这么多步（和 axonkey 一样），防止配错的长序列塞满内存
const MAX_QUEUE: usize = 256;

static QUEUE: LazyLock<Mutex<Queue>> = LazyLock::new(Default::default);
static WAKE: Condvar = Condvar::new();

/// 把一串动作排进执行线程：停用的、「禁用」、看不懂的跳过；「等待」把后面的步骤往后推。返回排进去几步
pub fn submit(steps: &[Step], options: RunOptions) -> usize {
    let mut queue = QUEUE.lock().unwrap();
    if !queue.started {
        queue.started = true;
        std::thread::Builder::new().name("actions".into()).spawn(worker).ok();
    }
    let mut due = Instant::now();
    let mut added = 0;
    for step in steps.iter().filter(|s| s.enabled) {
        match &step.action {
            Action::Delay { ms } => due += Duration::from_millis(*ms),
            // 「禁用」、看不懂的、还没选按键的步骤什么都不做
            Action::Disabled | Action::Unknown(_) => {}
            action if action.is_blank() => {}
            action => {
                if queue.jobs.len() >= MAX_QUEUE {
                    log!("⚠ 排队的动作太多（{MAX_QUEUE} 步），后面的不执行了");
                    break;
                }
                let order = queue.next_order;
                queue.next_order += 1;
                queue.jobs.push(Job { due, order, action: action.clone(), options });
                added += 1;
            }
        }
    }
    // 按时间排，同一时间的保持提交顺序
    queue.jobs.sort_by_key(|job| (job.due, job.order));
    WAKE.notify_all();
    added
}

/// 取消还没执行的步骤，返回取消了几步
pub fn cancel() -> usize {
    let mut queue = QUEUE.lock().unwrap();
    let dropped = queue.jobs.len();
    queue.jobs.clear();
    dropped
}

fn worker() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let mut queue = QUEUE.lock().unwrap();
    loop {
        let now = Instant::now();
        match queue.jobs.first() {
            None => queue = WAKE.wait(queue).unwrap(),
            Some(job) if job.due > now => {
                let wait = job.due - now;
                queue = WAKE.wait_timeout(queue, wait).unwrap().0;
            }
            Some(_) => {
                let job = queue.jobs.remove(0);
                drop(queue);
                let late = Instant::now().saturating_duration_since(job.due);
                if job.options.stale_after.is_some_and(|limit| late > limit) {
                    // 连发的这一下已经过时：跳过
                } else {
                    execute(&job.action, &job.options);
                }
                queue = QUEUE.lock().unwrap();
            }
        }
    }
}

/// 切到上一个 / 下一个方案
fn switch_profile(direction: &str, wrap: bool) {
    match state::switch_profile(direction, wrap) {
        Ok(Some((from, to))) => {
            let dropped = cancel();
            input::release_keys_all();
            link::push_config();
            link::send_to_phone(&serde_json::json!({"t": "profile", "from": from, "to": to}));
            overlay::show(&from, &to);
            log!("🔀 切换方案：{from} → {to}{}", if dropped > 0 { format!("（取消了 {dropped} 步还没执行的动作）") } else { String::new() });
        }
        Ok(None) => log!("⚠ 没有可以切换的方案（只有一个方案，或者已经到头了）"),
        Err(e) => log!("⚠ 切换方案失败：{e}"),
    }
}

fn execute(action: &Action, options: &RunOptions) {
    match action {
        Action::SwitchProfile { direction, wrap } => switch_profile(direction, *wrap),
        Action::Key { key } => {
            if !input::key(key, &[]) {
                log!("⚠ 不认识的按键：{key}");
            }
        }
        Action::Shortcut { keys } => match input::key_codes(keys) {
            Some(vks) => input::tap(&vks),
            None => log!("⚠ 不认识的组合键：{}", keys.join("+")),
        },
        Action::Mouse { button } => {
            if let Some(b) = input::Button::parse(button) {
                input::button(b, "click");
            }
        }
        Action::Wheel { direction } => {
            let (dx, dy) = match direction.as_str() {
                "up" => (0, 120),
                "down" => (0, -120),
                "left" => (-120, 0),
                _ => (120, 0),
            };
            input::scroll(dx, dy);
        }
        Action::CursorMove { direction, distance } => {
            let d = (*distance).clamp(1, phonepad_config::actions::MAX_CURSOR_DISTANCE) as f64 * options.cursor_scale;
            let (dx, dy) = match direction.as_str() {
                "up" => (0.0, -d),
                "down" => (0.0, d),
                "left" => (-d, 0.0),
                _ => (d, 0.0),
            };
            input::move_now(dx, dy);
        }
        Action::Paste { text, mode } => {
            let clipboard = match mode {
                Some(PasteMode::Clipboard) => true,
                Some(PasteMode::Type) => false,
                None => options.paste_by_clipboard,
            };
            if clipboard {
                input::paste_text(text)
            } else {
                input::type_text(text)
            }
        }
        Action::OpenApp { path, args, activate } => {
            if let Err(e) = launcher::open_app(path, args, *activate) {
                log!("⚠ 打开应用失败：{e}");
            }
        }
        Action::OpenWebsite { url } => {
            if let Err(e) = launcher::open_website(url) {
                log!("⚠ 打开网站失败：{e}");
            }
        }
        Action::Delay { .. } | Action::Disabled | Action::Unknown(_) => {}
    }
}
