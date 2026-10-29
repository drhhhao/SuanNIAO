//! 蓝牙串口（RFCOMM）服务：注册服务、接受手机连接、按行收发 JSON 消息。
//!
//! 手机 → 电脑：hello（device 是手机的蓝牙名，model 是型号，app 是 App 版本）、ping、
//!              move/scroll/mouse（触控板；move 带 ts＝手指移动的时间，电脑按它匀速回放，mouse 和 move 按先后顺序发）、
//!              button（按键 id + 按法；连发、重复长按时带 seq、ts、held）、hold（直通的按键按下 / 松开）、text（语音文字）、
//!              state（亮屏/息屏）、layout（手机上调好的布局）、battery（电量 level 0～100、charging，连上时和变化时发）、
//!              configAck（套用完第 rev 次推送的配置）、setEnabled（手机上暂停 / 恢复按键）
//! 电脑 → 手机：hello、pong、config（布局和参数，连上时和配置变化时发，带每次加一的 rev）、layoutSaved（布局存好没有）、
//!              profile（切换了方案：from、to）

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use phonepad_config::{self as config, HoldMode, Rect, Trigger};
use serde_json::{json, Value};
use windows::core::{Result, GUID, HSTRING};
use windows::Devices::Bluetooth::Rfcomm::{RfcommServiceId, RfcommServiceProvider};
use windows::Foundation::TypedEventHandler;
use windows::Networking::Sockets::{
    SocketProtectionLevel, StreamSocket, StreamSocketListener, StreamSocketListenerConnectionReceivedEventArgs,
};
use windows::Storage::Streams::{Buffer, DataReader, DataWriter, InputStreamOptions};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

use crate::util::preview;
use crate::{actions, input, log, state, tray};

/// 手机按这个服务 ID 找到电脑上的服务。
pub const SERVICE_UUID: GUID = GUID::from_u128(0x5f6a1c2e_8b3d_4e7f_9a10_2b3c4d5e6f70);
const SERVICE_NAME: &str = "PhonePad";

/// 只保留一部手机：新连接会顶掉旧连接。
static ACTIVE: Mutex<Option<StreamSocket>> = Mutex::new(None);
/// 当前手机的连接；所有发给手机的消息都经过这把锁，保证一条一条地写。
static PHONE: Mutex<Option<StreamSocket>> = Mutex::new(None);

pub struct Server {
    _provider: RfcommServiceProvider,
    _listener: StreamSocketListener,
}

impl Server {
    pub fn start() -> Result<Self> {
        let provider = RfcommServiceProvider::CreateAsync(&RfcommServiceId::FromUuid(SERVICE_UUID)?)?.join()?;
        let listener = StreamSocketListener::new()?;
        listener.ConnectionReceived(&TypedEventHandler::<
            StreamSocketListener,
            StreamSocketListenerConnectionReceivedEventArgs,
        >::new(|_listener, args| {
            let socket = args.ok()?.Socket()?;
            std::thread::spawn(move || Session::run(socket));
            Ok(())
        }))?;
        listener
            .BindServiceNameWithProtectionLevelAsync(
                &provider.ServiceId()?.AsString()?,
                SocketProtectionLevel::BluetoothEncryptionWithAuthentication,
            )?
            .join()?;

        // SDP 里写上服务名（属性 0x100，文本类型、1 字节长度）
        let sdp = DataWriter::new()?;
        sdp.WriteByte((4 << 3) | 5)?;
        sdp.WriteByte(SERVICE_NAME.len() as u8)?;
        sdp.WriteString(&HSTRING::from(SERVICE_NAME))?;
        provider.SdpRawAttributes()?.Insert(0x100, &sdp.DetachBuffer()?)?;
        provider.StartAdvertising(&listener)?;
        Ok(Self { _provider: provider, _listener: listener })
    }
}

/// 发一条消息给手机；没连着就丢掉。
pub fn send_to_phone(value: &Value) -> bool {
    let guard = PHONE.lock().unwrap();
    let Some(socket) = guard.as_ref() else { return false };
    let mut bytes = value.to_string().into_bytes();
    bytes.push(b'\n');
    let result = (|| -> Result<()> {
        let output = socket.OutputStream()?;
        let writer = DataWriter::new()?;
        writer.WriteBytes(&bytes)?;
        output.WriteAsync(&writer.DetachBuffer()?)?.join()?;
        Ok(())
    })();
    result.is_ok()
}

/// 配置推送的序号：每推一次加一，手机套用完回 configAck 带上它
static CONFIG_REV: AtomicU64 = AtomicU64::new(0);

/// 把当前配置（布局、参数）发给手机，带上新的 rev；真的发出去了才记进状态（主界面看回执用）
pub fn push_config() {
    let rev = CONFIG_REV.fetch_add(1, Ordering::Relaxed) + 1;
    let mut payload = state::config().phone_payload();
    payload["rev"] = json!(rev);
    if send_to_phone(&payload) {
        state::update_status(|s| {
            s.config_rev = rev;
            s.config_at = config::now_secs();
        });
    }
}

/// 看门狗要用的状态：亮屏时 10 秒收不到消息就松开按住的键。
struct Watch {
    alive: bool,
    screen_on: bool,
    last_rx: Instant,
}

struct Session {
    moves: Vec<Instant>,
    stats_since: Instant,
    /// 正在按住连发的那个键：按下那一下在手机上的时刻（ts）和到这边的时刻，用来发现处理跟不上
    burst: Option<Burst>,
    /// 直通：每个手机按键按下时按着的键（松开时照这个松，中途改了配置也不会松错）
    holds: HashMap<String, (Vec<u16>, u64)>,
    /// 暂停时按了键：提醒过一次就不再刷日志
    paused_noted: bool,
}

struct Burst {
    id: String,
    phone_ms: f64,
    at: Instant,
    skipped: u32,
}

/// 连发的某一下比该执行的时刻晚了这么多就跳过：动作慢（比如一串动作、打开程序）时不越积越多，
/// 松手后也不会还在这边接着按
const REPEAT_MAX_LAG_MS: f64 = 250.0;

impl Session {
    fn run(socket: StreamSocket) {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        if let Some(old) = ACTIVE.lock().unwrap().replace(socket.clone()) {
            let _ = old.Close();
        }
        let peer = socket
            .Information()
            .and_then(|info| info.RemoteHostName())
            .and_then(|host| host.DisplayName())
            .map(|name| name.to_string())
            .unwrap_or_else(|_| "?".into());
        *PHONE.lock().unwrap() = Some(socket.clone());
        log!("📱 手机已连接：{peer}");
        state::update_status(|s| {
            s.connected = true;
            s.since = config::now_secs();
        });

        let started = Instant::now();
        let watch = Arc::new(Mutex::new(Watch { alive: true, screen_on: true, last_rx: Instant::now() }));
        spawn_watchdog(watch.clone());
        let result = Self::serve(&socket, &watch);
        watch.lock().unwrap().alive = false;
        // 断线：还没执行的动作不做了，按着的键都松开
        actions::cancel();
        input::release_all();

        // 被新连接顶掉时不要清掉新连接的状态
        let still_current = ACTIVE.lock().unwrap().as_ref().map(|s| s == &socket).unwrap_or(false);
        if still_current {
            *ACTIVE.lock().unwrap() = None;
            *PHONE.lock().unwrap() = None;
            state::update_status(|s| {
                s.connected = false;
                s.battery = None;
            });
            tray::set_tooltip("蒜鸟 · 未连接");
        }
        let reason = match result {
            Ok(()) => "对方关闭了连接".to_string(),
            Err(e) => e.message().to_string(),
        };
        log!("👋 手机断开（连了 {:.0} 秒）：{reason}", started.elapsed().as_secs_f64());
    }

    fn serve(socket: &StreamSocket, watch: &Arc<Mutex<Watch>>) -> Result<()> {
        let input_stream = socket.InputStream()?;
        let mut session = Session { moves: Vec::new(), stats_since: Instant::now(), burst: None, holds: HashMap::new(), paused_noted: false };
        let buffer = Buffer::Create(4096)?;
        let mut pending: Vec<u8> = Vec::new();
        loop {
            let data = input_stream.ReadAsync(&buffer, 4096, InputStreamOptions::Partial)?.join()?;
            let len = data.Length()? as usize;
            if len == 0 {
                return Ok(());
            }
            crate::trace::event("rx", &[len as f64]);
            let mut chunk = vec![0u8; len];
            DataReader::FromBuffer(&data)?.ReadBytes(&mut chunk)?;
            pending.extend_from_slice(&chunk);
            while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = pending.drain(..=pos).collect();
                watch.lock().unwrap().last_rx = Instant::now();
                session.handle(&line[..line.len() - 1], watch);
            }
            if pending.len() > 1 << 20 {
                pending.clear(); // 异常数据，丢掉，别撑爆内存
            }
        }
    }

    fn handle(&mut self, line: &[u8], watch: &Arc<Mutex<Watch>>) {
        let Ok(msg) = serde_json::from_slice::<Value>(line) else {
            log!("⚠ 看不懂的消息：{}", String::from_utf8_lossy(line));
            return;
        };
        let num = |k: &str| msg.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        let text = |k: &str| msg.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        if crate::trace::dry_run() && matches!(text("t").as_str(), "button" | "hold" | "key" | "text" | "layout") {
            crate::trace::event("skip", &[]);
            return;
        }
        match text("t").as_str() {
            "hello" => {
                // 新版 App：device 是手机的蓝牙名（少爷的mi13），model 是型号；旧版 App 的 device 是「厂商 型号」
                let device = text("device");
                let model = text("model");
                let app = text("app");
                log!("🤝 {device}（{}App {app}）", if model.is_empty() { String::new() } else { format!("{model}，") });
                state::update_status(|s| {
                    s.device = device.clone();
                    s.phone_model = model.clone();
                    s.phone_app = app.clone();
                    // 新的连接：回执重新算
                    s.ack_rev = 0;
                    s.ack_at = 0;
                });
                tray::set_tooltip(&format!("蒜鸟 · 已连接 · {device}"));
                send_to_phone(&json!({"t": "hello", "host": crate::util::host_name(), "app": env!("CARGO_PKG_VERSION")}));
                push_config();
            }
            "ping" => {
                send_to_phone(&json!({"t": "pong", "id": msg.get("id"), "ts": msg.get("ts")}));
            }
            "move" => {
                let ts = msg.get("ts").and_then(Value::as_f64);
                crate::trace::event("move", &[num("x"), num("y"), ts.unwrap_or(-1.0)]);
                input::move_by(num("x").clamp(-3000.0, 3000.0), num("y").clamp(-3000.0, 3000.0), ts);
                self.note_move();
            }
            "scroll" => input::scroll(num("x").clamp(-12000.0, 12000.0) as i32, num("y").clamp(-12000.0, 12000.0) as i32),
            "mouse" => {
                crate::trace::event("mouse", &[match text("a").as_str() { "down" => 1.0, "up" => 0.0, _ => 2.0 }]);
                if let Some(button) = input::Button::parse(&text("b")) {
                    input::button(button, &text("a"));
                }
            }
            "button" => {
                let seq = msg.get("seq").and_then(Value::as_u64);
                let ts = msg.get("ts").and_then(Value::as_f64);
                let held = msg.get("held").and_then(Value::as_f64);
                self.press_button(&text("id"), &text("trigger"), seq, ts, held);
            }
            "hold" => {
                let down = msg.get("down").and_then(Value::as_bool).unwrap_or(false);
                if down {
                    self.hold_down(&text("id"));
                } else {
                    self.hold_up(&text("id"));
                }
            }
            "layout" => save_layout(&msg),
            "configAck" => {
                let rev = msg.get("rev").and_then(Value::as_u64).unwrap_or(0);
                state::update_status(|s| {
                    s.ack_rev = rev;
                    s.ack_at = config::now_secs();
                });
            }
            "setEnabled" => match msg.get("enabled").and_then(Value::as_bool) {
                Some(enabled) => tray::set_enabled(enabled, "手机"),
                None => log!("⚠ setEnabled 没带 enabled"),
            },
            "battery" => {
                let level = num("level").round().clamp(0.0, 100.0) as u8;
                let charging = msg.get("charging").and_then(Value::as_bool).unwrap_or(false);
                let battery = config::Battery { level, charging };
                let changed = state::status().battery != Some(battery);
                state::update_status(|s| s.battery = Some(battery));
                if changed {
                    log!("🔋 手机电量 {level}%{}", if charging { "，充电中" } else { "" });
                }
            }
            "key" => {
                let mods: Vec<String> = msg
                    .get("m")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                if !input::key(&text("k"), &mods) {
                    log!("⚠ 不认识的按键：{}", text("k"));
                } else if mods.is_empty() && text("k") == "backspace" {
                    state::record_backspace(); // 边说边打改字
                }
            }
            "text" => {
                let settings = state::config().settings;
                let s = text("s");
                let paste = msg.get("mode").and_then(Value::as_str).map(|m| m == "paste").unwrap_or(settings.voice.paste);
                let enter = msg.get("enter").and_then(Value::as_bool).unwrap_or(settings.voice.enter_after);
                if !s.is_empty() {
                    if paste { input::paste_text(&s) } else { input::type_text(&s) }
                }
                if enter {
                    std::thread::sleep(Duration::from_millis(30));
                    input::key("enter", &[]);
                }
                log!(
                    "⌨ {}{} {} 字：{}",
                    if paste { "粘贴" } else { "逐字输入" },
                    if enter { " + 回车" } else { "" },
                    s.chars().count(),
                    preview(&s)
                );
                state::record_text(&s);
            }
            "state" => {
                let on = text("screen") == "on";
                watch.lock().unwrap().screen_on = on;
                if !on {
                    input::release_all();
                }
                log!("{}", if on { "🔆 手机亮屏" } else { "🌙 手机息屏（连接保持）" });
            }
            "release" => input::release_all(),
            "smooth" => {} // 旧版 App 的开关，现在以电脑端设置为准
            "mem" => {
                let (working_set, private) = crate::util::memory_bytes();
                send_to_phone(&json!({"t": "mem", "ws": working_set, "private": private}));
            }
            other => log!("⚠ 不认识的消息类型：{other}"),
        }
    }

    /// 暂停按键映射时不执行（触控板、语音照常）
    fn paused(&mut self, config: &config::Config) -> bool {
        if config.settings.enabled {
            self.paused_noted = false;
            return false;
        }
        if !self.paused_noted {
            self.paused_noted = true;
            log!("⏸ 按键映射暂停中，手机上的按键不执行");
        }
        true
    }

    /// 执行按键的动作。seq 是连发 / 重复长按的第几下（0 = 第一下），ts 是手机发出时的时刻（毫秒），
    /// held 是手机上已经按住了多久（毫秒，连发光标移动时加速用）。
    fn press_button(&mut self, id: &str, trigger: &str, seq: Option<u64>, ts: Option<f64>, held: Option<f64>) {
        let Some(trigger) = Trigger::parse(trigger) else { return };
        let config = state::config();
        if self.paused(&config) {
            return;
        }
        let Some(module) = config.find_module(id) else {
            log!("⚠ 配置里没有按键 {id}");
            return;
        };
        let hold = module.hold_mode();
        if !hold.judges_other_triggers() && trigger != Trigger::Click {
            log!("⚠ {} 按住时是「{}」，不判定{}", module.title(), hold.name(), trigger.name());
            return;
        }
        if let (Some(n), Some(ts)) = (seq, ts) {
            if n == 0 || self.burst.as_ref().is_none_or(|b| b.id != id) {
                self.end_burst();
                self.burst = Some(Burst { id: id.to_string(), phone_ms: ts, at: Instant::now(), skipped: 0 });
            } else if let Some(burst) = self.burst.as_mut() {
                let lag = burst.at.elapsed().as_secs_f64() * 1000.0 - (ts - burst.phone_ms);
                if lag > REPEAT_MAX_LAG_MS {
                    burst.skipped += 1;
                    return;
                }
            }
        }
        let steps = module.behaviors.get(trigger);
        let summary = config::summarize(steps);
        let repeated = seq.is_some_and(|n| n > 0);
        let how = match (repeated, trigger) {
            (true, Trigger::LongPress) => "重复长按",
            (true, _) => "连发",
            (false, t) => t.name(),
        };
        match seq {
            Some(1) => log!("🔁 {} · {how} → {summary}", module.title()),
            Some(n) if n > 1 => {}
            _ => log!("🔘 {} · {} → {summary}", module.title(), trigger.name()),
        }
        let t = &config.settings.triggers;
        let mut options = actions::RunOptions::new(config.settings.voice.paste);
        if repeated {
            // 过时就跳过，和上面按手机时刻判断的合起来：动作慢时不越积越多
            options.stale_after = Some(Duration::from_millis(REPEAT_MAX_LAG_MS as u64));
            // 连发光标移动：按住 3 秒内线性加速到 (1 + 加速度/100) 倍（从开始连发算起）
            if trigger == Trigger::Click {
                let since = (held.unwrap_or(0.0) - t.repeat_delay_ms as f64).clamp(0.0, 3000.0);
                options.cursor_scale = 1.0 + t.cursor_acceleration.min(100) as f64 / 100.0 * since / 3000.0;
            }
        }
        actions::submit(steps, options);
        state::record_button(id, module.title(), format!("{how} → {summary}"), seq);
    }

    /// 直通的按键按下：按下单击里的键并一直按着，直到手机说松开（一次按住统计里只算一次）
    fn hold_down(&mut self, id: &str) {
        let config = state::config();
        if self.paused(&config) {
            return;
        }
        let Some(module) = config.find_module(id) else {
            log!("⚠ 配置里没有按键 {id}");
            return;
        };
        if self.holds.contains_key(id) {
            return; // 重复的按下（不该有），别多按一次
        }
        let keys = match module.passthrough_keys().filter(|_| module.hold_mode() == HoldMode::Passthrough) {
            Some(keys) => keys,
            None => {
                log!("⚠ {} 不是直通的按键，忽略按住", module.title());
                return;
            }
        };
        let Some(vks) = input::key_codes(&keys) else {
            log!("⚠ 不认识的按键：{}", keys.join("+"));
            return;
        };
        let generation = input::hold_keys(&vks);
        self.holds.insert(id.to_string(), (vks, generation));
        log!("⏬ {} · 按住 {}", module.title(), keys.join("+"));
        state::record_button(id, module.title(), format!("直通 → {}", keys.join("+")), None);
    }

    fn hold_up(&mut self, id: &str) {
        if let Some((vks, generation)) = self.holds.remove(id) {
            input::release_keys(&vks, generation);
        }
    }

    /// 上一次连发结束：跟不上而跳过了几下的话记一笔
    fn end_burst(&mut self) {
        if let Some(burst) = self.burst.take() {
            if burst.skipped > 0 {
                log!("⏩ {} 连发时动作跟不上，跳过了 {} 下", burst.id, burst.skipped);
            }
        }
    }

    /// 统计触控板消息到达的间隔，量化"跟不跟手"。每 3 秒记一次。
    fn note_move(&mut self) {
        let now = Instant::now();
        self.moves.push(now);
        if now.duration_since(self.stats_since) < Duration::from_secs(3) {
            return;
        }
        let mut gaps: Vec<f64> = self
            .moves
            .windows(2)
            .map(|w| w[1].duration_since(w[0]).as_secs_f64() * 1000.0)
            .filter(|gap| *gap < 500.0) // 两次滑动之间的停顿不算
            .collect();
        if gaps.len() >= 10 {
            gaps.sort_by(|a, b| a.total_cmp(b));
            let at = |q: f64| gaps[((gaps.len() - 1) as f64 * q).round() as usize];
            log!(
                "🖱 触控板（平滑{}）：3 秒内 {} 条，间隔 中位 {:.1} ms · P95 {:.1} ms · 最大 {:.1} ms{}",
                if input::smoothing() { "开" } else { "关" },
                self.moves.len(),
                at(0.5),
                at(0.95),
                gaps[gaps.len() - 1],
                if input::smoothing() { format!("；匀速回放多等 {:.0} ms", input::motion_wait()) } else { String::new() }
            );
        }
        self.moves.clear();
        self.stats_since = now;
    }
}

/// 手机上调好的布局：校验后写回配置文件，先把新配置推给手机，再告诉它存好了没有。
fn save_layout(msg: &Value) {
    let page = msg.get("page").and_then(Value::as_str).unwrap_or("");
    let items: Vec<(String, Rect)> = msg
        .get("modules")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|m| {
                    let n = |k: &str| m.get(k).and_then(Value::as_f64);
                    Some((m.get("id")?.as_str()?.to_string(), Rect::new(n("x")?, n("y")?, n("w")?, n("h")?)))
                })
                .collect()
        })
        .unwrap_or_default();
    let result = state::save_phone_layout(page, &items);
    match &result {
        Ok(n) => {
            log!("📐 手机上调整了布局（{page} 页，{n} 个模块），已写回配置");
            push_config();
        }
        Err(e) => log!("⚠ 手机上调的布局没保存：{e}"),
    }
    let error = result.err().unwrap_or_default();
    send_to_phone(&json!({"t": "layoutSaved", "ok": error.is_empty(), "error": error}));
}

fn spawn_watchdog(watch: Arc<Mutex<Watch>>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        let state = watch.lock().unwrap();
        if !state.alive {
            return;
        }
        if state.screen_on && state.last_rx.elapsed() > Duration::from_secs(10) && input::any_held() {
            input::release_all();
            log!("⏱ 10 秒没收到手机消息，已松开按着的鼠标键和键盘键");
        }
    });
}
