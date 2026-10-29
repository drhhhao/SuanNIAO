//! 全局状态：当前配置、给主界面看的运行状态。
//!
//! 配置文件读不出来时不换成默认值：开着的时候留着上一份好的配置；启动时就读不出来，用最近一份能读的备份
//! （只读，不覆盖坏文件），并在 status.json 里写上 configError，主界面据此显示横幅。坏文件另存一份带时间的。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, RwLock};
use std::time::SystemTime;

use phonepad_config::{self as config, Config, Rect, Recent, Status};

use crate::{input, log, util};

static CONFIG: LazyLock<RwLock<Config>> = LazyLock::new(|| RwLock::new(Config::default()));
static STATUS: LazyLock<Mutex<Status>> = LazyLock::new(|| Mutex::new(Status::default()));
/// 状态改了但还没写文件（连发时每秒几十下，攒一下再写）
static STATUS_DIRTY: AtomicBool = AtomicBool::new(false);
/// 后台自己写配置文件后的修改时间：看文件的线程见到它就知道不是别人改的，不用再读一遍
static OWN_WRITE: Mutex<Option<SystemTime>> = Mutex::new(None);

pub fn config() -> Config {
    CONFIG.read().unwrap().clone()
}

/// 启动时读配置：没有文件就写一份全新的；读不出来就用最近的备份（不写文件），记下 configError
pub fn load_config_at_start() {
    let (fresh, error) = match config::read_config() {
        config::Loaded::Missing => {
            let fresh = config::Config::fresh();
            if let Err(e) = config::save(&fresh) {
                log!("⚠ 写默认配置失败：{e}");
            }
            (fresh, None)
        }
        _ => config::load(),
    };
    match &error {
        Some(e) => log!("⚠ 配置文件读不出来（{}），另存为 {}，现在用{}", e.message, e.broken, e.using),
        None => remember_good(),
    }
    input::set_smoothing(fresh.settings.touchpad.smoothing);
    *CONFIG.write().unwrap() = fresh;
    update_status(|s| s.config_error = error);
}

/// 配置文件变了：读得出来就换上并返回 true；读不出来留着现在的（不推默认值给手机），记下 configError，返回 false
pub fn reload_config() -> bool {
    match config::read_config() {
        config::Loaded::Ok(fresh) => {
            input::set_smoothing(fresh.settings.touchpad.smoothing);
            *CONFIG.write().unwrap() = fresh;
            remember_good();
            if status().config_error.is_some() {
                log!("✅ 配置文件又能读了");
                update_status(|s| s.config_error = None);
            }
            true
        }
        config::Loaded::Missing => {
            log!("⚠ 配置文件被删了：先用现在的，主界面下次保存会写回去");
            false
        }
        config::Loaded::Broken { message, text } => {
            let broken = config::preserve_broken(&text).unwrap_or_default();
            let already = status().config_error.is_some_and(|e| e.broken == broken);
            if !already {
                log!("⚠ 配置文件读不出来（{message}），另存为 {broken}；先用上一份好的配置");
            }
            let error = config::ConfigError { message, broken, using: "上一份好的配置".into(), at: config::now_secs() };
            update_status(|s| s.config_error = Some(error));
            false
        }
    }
}

/// 读好的配置文件记一份自动备份（配置读坏时用来恢复）
fn remember_good() {
    if let Ok(text) = std::fs::read_to_string(config::config_path()) {
        config::remember_good(&text);
    }
}

/// 改状态并写到 status.json（顺便刷新时间戳和内存）。
pub fn update_status(change: impl FnOnce(&mut Status)) {
    let mut status = STATUS.lock().unwrap();
    change(&mut status);
    STATUS_DIRTY.store(false, Ordering::Relaxed);
    status.pid = std::process::id();
    status.version = env!("CARGO_PKG_VERSION").to_string();
    status.updated = config::now_secs();
    status.memory_mb = util::mb(util::memory_bytes().0);
    if let Ok(text) = serde_json::to_string(&*status) {
        let _ = config::write_json(&config::status_path(), &text);
    }
}

/// 后台重启后接着记：上次 status.json 里的最近动作、最后连的手机接回来，同一天的用量也接回来
pub fn restore_usage() {
    let Some(old) = config::read_status() else { return };
    let mut status = STATUS.lock().unwrap();
    status.recent = old.recent;
    status.recent.truncate(config::RECENT_MAX);
    // 上次连过的手机（关于页、手机信息浮层显示「最后连的是什么」）
    status.device = old.device;
    status.phone_model = old.phone_model;
    status.phone_app = old.phone_app;
    if old.day == util::local_day() {
        status.day = old.day;
        status.presses = old.presses;
        status.voice_chars = old.voice_chars;
        status.per_button = old.per_button;
    }
}

/// 换了一天：今天的用量清零（最近动作留着）
fn roll_day(status: &mut Status) {
    let today = util::local_day();
    if status.day != today {
        status.day = today;
        status.presses = 0;
        status.voice_chars = 0;
        status.per_button.clear();
    }
}

/// 现在的运行状态（拷一份）
pub fn status() -> Status {
    STATUS.lock().unwrap().clone()
}

/// 只改内存里的状态，文件由 flush_status 稍后统一写（main 里每 200 毫秒看一次）
fn update_status_soon(change: impl FnOnce(&mut Status)) {
    change(&mut STATUS.lock().unwrap());
    STATUS_DIRTY.store(true, Ordering::Relaxed);
}

/// 有攒着没写的状态就写到文件
pub fn flush_status() {
    if STATUS_DIRTY.load(Ordering::Relaxed) {
        update_status(|_| {});
    }
}

/// 记一次按键：今天的次数加一，「最近动作」里加一条。
/// repeat：按住连发的第几下（0 = 按下那一下，None = 普通按法）。连发的后面几下每一下都算次数，
/// 但「最近动作」里合进按下时那一条（界面上显示「×N」），不刷屏；状态文件也攒一下再写。
pub fn record_button(id: &str, title: String, detail: String, repeat: Option<u64>) {
    let follow_up = repeat.is_some_and(|n| n > 0);
    let change = |s: &mut Status| {
        roll_day(s);
        s.presses += 1;
        *s.per_button.entry(id.to_string()).or_default() += 1;
        let now = config::now_secs();
        match s.recent.first_mut() {
            Some(last) if follow_up && last.kind == "key" && last.id == id => {
                last.count = last.count.max(1) + 1;
                last.detail = detail;
                last.at = now;
            }
            _ => {
                s.recent.insert(0, Recent { kind: "key".into(), id: id.to_string(), title, detail, at: now, count: 1 });
                s.recent.truncate(config::RECENT_MAX);
            }
        }
    };
    if follow_up {
        update_status_soon(change);
    } else {
        update_status(change);
    }
}

/// 边说边打会一小段一小段地发：3 秒内接着来的算同一句，合成一条
const VOICE_MERGE_SECS: u64 = 3;

/// 记一段语音文字
pub fn record_text(text: &str) {
    let chars = text.chars().count() as u32;
    if chars == 0 {
        return;
    }
    update_status(|s| {
        roll_day(s);
        s.voice_chars += chars;
        let now = config::now_secs();
        match s.recent.first_mut() {
            Some(last) if last.kind == "voice" && now.saturating_sub(last.at) <= VOICE_MERGE_SECS => {
                last.detail.push_str(text);
                last.at = now;
            }
            _ => {
                s.recent.insert(0, Recent { kind: "voice".into(), detail: text.to_string(), at: now, count: 1, ..Default::default() });
                s.recent.truncate(config::RECENT_MAX);
            }
        }
        if let Some(last) = s.recent.first_mut() {
            last.title = format!("语音 · {} 字", last.detail.chars().count());
        }
    });
}

/// 边说边打时识别结果改了，手机先发退格删掉旧字：刚说的那句也跟着删
pub fn record_backspace() {
    update_status(|s| {
        let now = config::now_secs();
        if let Some(last) = s.recent.first_mut() {
            if last.kind == "voice" && now.saturating_sub(last.at) <= VOICE_MERGE_SECS && last.detail.pop().is_some() {
                last.at = now;
                last.title = format!("语音 · {} 字", last.detail.chars().count());
                s.voice_chars = s.voice_chars.saturating_sub(1);
            }
        }
    });
}

/// 手机上调好的布局：以文件里现在的配置为准（主界面可能刚改过、这边还没读到），改好位置写回文件。
/// 写完马上换成新配置；看文件的线程认得这次修改是自己写的，不会再读一遍。
pub fn save_phone_layout(page: &str, items: &[(String, Rect)]) -> Result<usize, String> {
    let mut fresh = config::load_existing().ok_or("电脑上的配置文件读不出来")?;
    let applied = fresh.set_page_layout(page, items)?;
    write_own(fresh)?;
    Ok(applied)
}

/// 「切换方案」动作：以文件为准，切到上一个 / 下一个方案后写回。返回（原来的名字，新的名字）；没得切返回 None
pub fn switch_profile(direction: &str, wrap: bool) -> Result<Option<(String, String)>, String> {
    let mut fresh = config::load_existing().ok_or("电脑上的配置文件读不出来")?;
    let Some(target) = fresh.adjacent_profile(direction, wrap) else { return Ok(None) };
    let names = fresh.switch_profile(&target);
    write_own(fresh)?;
    Ok(names)
}

/// 托盘菜单、手机上的「暂停按键 / 恢复按键」：同样以文件为准，只改总开关
pub fn set_mapping_enabled(enabled: bool) -> Result<(), String> {
    let mut fresh = config::load_existing().ok_or("电脑上的配置文件读不出来")?;
    fresh.settings.enabled = enabled;
    write_own(fresh)
}

/// 后台自己写配置文件：写完马上换成新配置，并记下修改时间（看文件的线程见到就知道不用再读）
fn write_own(fresh: Config) -> Result<(), String> {
    config::save(&fresh).map_err(|e| format!("写配置文件失败：{e}"))?;
    *OWN_WRITE.lock().unwrap() = std::fs::metadata(config::config_path()).and_then(|m| m.modified()).ok();
    input::set_smoothing(fresh.settings.touchpad.smoothing);
    *CONFIG.write().unwrap() = fresh;
    Ok(())
}

/// 配置文件的这次修改是不是后台自己写的（是的话顺便清掉记号）
pub fn is_own_write(modified: Option<SystemTime>) -> bool {
    let mut own = OWN_WRITE.lock().unwrap();
    let mine = modified.is_some() && *own == modified;
    if mine {
        *own = None;
    }
    mine
}

/// 退出时把状态标成"已停止"，主界面就不会以为后台还在。
pub fn mark_stopped() {
    let mut status = STATUS.lock().unwrap();
    status.connected = false;
    status.updated = 0;
    if let Ok(text) = serde_json::to_string(&*status) {
        let _ = config::write_json(&config::status_path(), &text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 配置文件读坏了：后台留着上一份好的配置，不改坏文件、另存一份；重新启动时用最近的自动备份；修好了横幅消失。
    /// 用临时文件夹当 APPDATA（只有这个测试读写配置文件）
    #[test]
    fn broken_config_keeps_the_last_good_one() {
        let dir = std::env::temp_dir().join(format!("phonepad-statetest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("APPDATA", &dir);
        let path = config::config_path();

        // 第一次用：没有文件就写一份全新的
        load_config_at_start();
        assert!(path.exists());
        assert!(status().config_error.is_none());
        let mut good = config();
        good.layout.pages[0].name = "好的那份".into();
        config::save(&good).unwrap();
        assert!(reload_config());
        assert_eq!(config().layout.pages[0].name, "好的那份");
        assert!(config::backups_dir().read_dir().unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("config-auto-")));

        // 写坏：不换配置、不动坏文件，另存一份 config.broken-时间.json，状态里有 configError
        let broken = "{\n  \"version\": 4,\n  \"layout\": {";
        std::fs::write(&path, broken).unwrap();
        assert!(!reload_config());
        assert_eq!(config().layout.pages[0].name, "好的那份");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
        let error = status().config_error.expect("configError");
        assert!(error.message.contains("文件不完整"), "{}", error.message);
        assert!(error.broken.starts_with("config.broken-") && config::data_dir().join(&error.broken).exists());
        assert_eq!(error.using, "上一份好的配置");

        // 坏着重新启动：用最近的自动备份，坏文件还是不动
        *CONFIG.write().unwrap() = config::Config::default();
        load_config_at_start();
        assert_eq!(config().layout.pages[0].name, "好的那份");
        assert!(status().config_error.expect("configError").using.starts_with("备份 config-auto-"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);

        // 修好了：换上、横幅消失
        config::save(&good).unwrap();
        assert!(reload_config());
        assert!(status().config_error.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
