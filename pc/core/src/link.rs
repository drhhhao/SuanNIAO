//! 和手机的连接：蓝牙串口（RFCOMM）服务、局域网（见 lan.rs）接进来的连接都在这里跑会话，按行收发 JSON 消息。
//! 两种连接可以同时连着：消息照样从两边收，发给手机的走局域网（断了就走蓝牙）。
//!
//! 手机 → 电脑：hello（device 是手机的蓝牙名，model 是型号，app 是 App 版本）、ping、
//!              move/scroll/mouse（触控板；move 带 ts＝手指移动的时间，电脑按它匀速回放，mouse 和 move 按先后顺序发）、
//!              button（按键 id + 按法；连发、重复长按时带 seq、ts、held）、hold（直通的按键按下 / 松开）、text（语音文字）、
//!              state（亮屏/息屏）、layout（手机上调好的布局）、battery（电量 level 0～100、charging，连上时和变化时发）、
//!              configAck（套用完第 rev 次推送的配置）、setEnabled（手机上暂停 / 恢复按键）、
//!              clip（手机剪贴板里的文字 s，回到蒜鸟时剪贴板变了才发，见 clipsync）
//! 电脑 → 手机：hello（带 pc＝局域网的电脑编号）、pong、config（布局和参数，连上时和配置变化时发，带每次加一的 rev）、
//!              layoutSaved（布局存好没有）、profile（切换了方案：from、to）、clip（电脑上复制的文字 s）、
//!              clipImg / clipImgPart / clipImgEnd / clipImgCancel（电脑上复制的图片，分块发，见 clipsync）、
//!              lan（局域网地址变了：pc、addrs、port、fp，手机更新记着的地址，见 lan.rs）

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use suanniao_config::{self as config, HoldMode, Rect, Trigger};
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
const SERVICE_NAME: &str = "SuanNIAO";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Bt,
    Lan,
}

impl Kind {
    fn code(self) -> &'static str {
        match self {
            Kind::Bt => "bt",
            Kind::Lan => "lan",
        }
    }
    fn name(self) -> &'static str {
        match self {
            Kind::Bt => "蓝牙",
            Kind::Lan => "局域网",
        }
    }
}

/// 一条连接：蓝牙的 StreamSocket，或者局域网的加密连接（带认证出来的手机编号）
#[derive(Clone)]
pub enum Conn {
    Bt(StreamSocket),
    Lan { conn: suanniao_lan::SecureConn, phone: String },
}

impl Conn {
    fn kind(&self) -> Kind {
        match self {
            Conn::Bt(_) => Kind::Bt,
            Conn::Lan { .. } => Kind::Lan,
        }
    }

    fn write(&self, bytes: &[u8]) -> bool {
        match self {
            Conn::Bt(socket) => (|| -> Result<()> {
                let output = socket.OutputStream()?;
                let writer = DataWriter::new()?;
                writer.WriteBytes(bytes)?;
                output.WriteAsync(&writer.DetachBuffer()?)?.join()?;
                Ok(())
            })()
            .is_ok(),
            Conn::Lan { conn, .. } => conn.write_all(bytes).is_ok(),
        }
    }

    fn close(&self) {
        match self {
            Conn::Bt(socket) => {
                let _ = socket.Close();
            }
            Conn::Lan { conn, .. } => conn.shutdown(),
        }
    }

    fn peer(&self) -> String {
        match self {
            Conn::Bt(socket) => socket
                .Information()
                .and_then(|info| info.RemoteHostName())
                .and_then(|host| host.DisplayName())
                .map(|name| name.to_string())
                .unwrap_or_else(|_| "?".into()),
            Conn::Lan { conn, .. } => conn.peer_addr().map(|a| a.ip().to_string()).unwrap_or_else(|| "?".into()),
        }
    }
}

struct Link {
    id: u64,
    conn: Conn,
    /// 哪部手机（扫码绑定时的编号）：局域网认证时就知道；蓝牙要等 hello 里带上（没扫码绑定过的手机不带）
    phone: Option<String>,
}

/// 连着的手机：每种连接最多一条，同一部手机的新连接顶掉同一种的旧连接；别的手机来连局域网会被拒（见 busy）
static LINKS: Mutex<Vec<Link>> = Mutex::new(Vec::new());
static NEXT_LINK: AtomicU64 = AtomicU64::new(1);
/// 发给手机的消息都经过这把锁，保证一条一条地写
static SEND: Mutex<()> = Mutex::new(());
/// 直通按住的键：每个手机按键按下时按着的键（松开时照这个松，中途改了配置也不会松错）。
/// 所有连接共用一份：按下走蓝牙、松开走局域网（中途换了连接）也能松开
static HOLDS: Mutex<Option<HashMap<String, (Vec<u16>, u64)>>> = Mutex::new(None);
/// 任意一条连接最近收到消息的时刻（看门狗：所有连接都没动静才松开按着的键）
static LAST_RX: Mutex<Option<Instant>> = Mutex::new(None);

/// 发消息用的连接，按顺序：局域网优先（快），然后蓝牙
fn preferred_all() -> Vec<Conn> {
    let links = LINKS.lock().unwrap();
    let mut list: Vec<&Link> = links.iter().collect();
    list.sort_by_key(|l| if l.conn.kind() == Kind::Lan { 0 } else { 1 });
    list.into_iter().map(|l| l.conn.clone()).collect()
}

/// 松开所有按着的鼠标键、键盘键，并清掉直通记录
fn release_everything() {
    HOLDS.lock().unwrap().get_or_insert_with(HashMap::new).clear();
    input::release_all();
}

/// 别的手机正连着（编号不一样）：只看知道编号、而且编号还在绑定列表里的连接。
/// 同一部手机重新扫码后编号换了，旧编号的连接可能还没断干净，不能把它当成另一部手机
fn other_phone(links: &[Link], phone: &str, except: u64) -> bool {
    links
        .iter()
        .any(|l| l.id != except && l.phone.as_deref().is_some_and(|p| p != phone && crate::lan::known_phone(p)))
}

/// 现在连着的方式，局域网在前
fn link_codes(links: &[Link]) -> Vec<String> {
    let mut kinds: Vec<Kind> = links.iter().map(|l| l.conn.kind()).collect();
    kinds.sort_by_key(|k| if *k == Kind::Lan { 0 } else { 1 });
    kinds.into_iter().map(|k| k.code().to_string()).collect()
}

/// 局域网连着的是哪部手机（扫码绑定时的编号），没有是 None
pub fn lan_phone() -> Option<String> {
    LINKS.lock().unwrap().iter().find_map(|l| match &l.conn {
        Conn::Lan { phone, .. } => Some(phone.clone()),
        _ => None,
    })
}

/// 解除绑定了这部手机：它正用局域网连着就断开
pub fn drop_lan_phone(id: &str) {
    for link in LINKS.lock().unwrap().iter() {
        if let Conn::Lan { phone, .. } = &link.conn {
            if phone == id {
                link.conn.close();
            }
        }
    }
}

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
            std::thread::spawn(move || Session::run(Conn::Bt(socket)));
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

/// 发一条消息给手机；没连着就丢掉。局域网写不出去（多半已经断了）就关掉它，改走蓝牙
pub fn send_to_phone(value: &Value) -> bool {
    let _guard = SEND.lock().unwrap();
    let mut bytes = value.to_string().into_bytes();
    bytes.push(b'\n');
    for conn in preferred_all() {
        if conn.write(&bytes) {
            return true;
        }
        if conn.kind() == Kind::Lan {
            conn.close();
        }
    }
    false
}

/// 回复走收到请求的那条连接（hello、pong、mem、layoutSaved）：另一条可能已经断了，而这条肯定刚收到过东西
fn reply(conn: &Conn, value: &Value) -> bool {
    let _guard = SEND.lock().unwrap();
    let mut bytes = value.to_string().into_bytes();
    bytes.push(b'\n');
    conn.write(&bytes)
}

/// 配置推送的序号：每推一次加一，手机套用完回 configAck 带上它
static CONFIG_REV: AtomicU64 = AtomicU64::new(0);

/// 把当前配置（布局、参数）发给手机，带上新的 rev；真的发出去了才记进状态（主界面看回执用）
pub fn push_config() {
    push_config_on(None);
}

/// via：手机刚打招呼的那条连接，配置从它发回去
fn push_config_on(via: Option<&Conn>) {
    let rev = CONFIG_REV.fetch_add(1, Ordering::Relaxed) + 1;
    let mut payload = state::config().phone_payload();
    payload["rev"] = json!(rev);
    let sent = match via {
        Some(conn) => reply(conn, &payload) || send_to_phone(&payload),
        None => send_to_phone(&payload),
    };
    if sent {
        state::update_status(|s| {
            s.config_rev = rev;
            s.config_at = config::now_secs();
        });
    }
}

/// 看门狗要用的状态：亮屏时这条局域网连接 5 秒收不到消息就断开它；所有连接 10 秒都没消息就松开按住的键。
struct Watch {
    alive: bool,
    screen_on: bool,
    last_rx: Instant,
    conn: Conn,
}

struct Session {
    moves: Vec<Instant>,
    stats_since: Instant,
    /// 正在按住连发的那个键：按下那一下在手机上的时刻（ts）和到这边的时刻，用来发现处理跟不上
    burst: Option<Burst>,
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
    fn run(conn: Conn) {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        let kind = conn.kind();
        let id = NEXT_LINK.fetch_add(1, Ordering::Relaxed);
        let peer = conn.peer();
        let phone = match &conn {
            Conn::Lan { phone, .. } => Some(phone.clone()),
            Conn::Bt(_) => None,
        };
        let first = {
            let mut links = LINKS.lock().unwrap();
            // 另一部绑定过的手机正连着：不顶掉它（两部手机在同一个 Wi-Fi 里会来回顶），告诉这部电脑忙
            if let Some(p) = &phone {
                if other_phone(&links, p, id) {
                    drop(links);
                    busy(&conn);
                    log!("🚫 另一部手机正连着，拒绝了局域网连接：{peer}");
                    return;
                }
            }
            // 同一种连接只留一条：新连接顶掉旧的
            links.retain(|l| {
                if l.conn.kind() == kind {
                    l.conn.close();
                    false
                } else {
                    true
                }
            });
            let first = links.is_empty();
            links.push(Link { id, conn: conn.clone(), phone });
            let codes = link_codes(&links);
            state::update_status(|s| {
                s.connected = true;
                if first {
                    s.since = config::now_secs();
                }
                s.links = codes;
            });
            first
        };
        log!("📱 手机已通过{}连接：{peer}{}", kind.name(), if first { "" } else { "（两种连接同时连着）" });
        if kind == Kind::Lan {
            crate::lan::refresh_status();
        }

        let started = Instant::now();
        let watch = Arc::new(Mutex::new(Watch { alive: true, screen_on: true, last_rx: Instant::now(), conn: conn.clone() }));
        spawn_watchdog(watch.clone());
        let result = Self::serve(&conn, &watch, id);
        watch.lock().unwrap().alive = false;
        conn.close();

        // 被同一种新连接顶掉时，列表里已经没有它了，不动新连接的状态
        let (was_listed, left) = {
            let mut links = LINKS.lock().unwrap();
            let before = links.len();
            links.retain(|l| l.id != id);
            (links.len() != before, link_codes(&links))
        };
        // 一条连接都不剩了：还没执行的动作不做了，按着的键都松开。
        // 另一条还连着就不动：手机会从那条接着发（松开也从那条来），断的这条上按着的键不会卡住
        if LINKS.lock().unwrap().is_empty() {
            actions::cancel();
            release_everything();
        }
        if was_listed {
            if left.is_empty() {
                state::update_status(|s| {
                    s.connected = false;
                    s.battery = None;
                    s.links.clear();
                });
                tray::set_tooltip("蒜鸟 · 未连接");
            } else {
                state::update_status(|s| s.links = left.clone());
            }
        }
        if kind == Kind::Lan {
            crate::lan::refresh_status();
        }
        log!(
            "👋 手机的{}连接断开（连了 {:.0} 秒）：{result}{}",
            kind.name(),
            started.elapsed().as_secs_f64(),
            if was_listed && !left.is_empty() { "；另一种连接还连着" } else { "" }
        );
    }

    /// 一直读到断开，返回断开的原因
    fn serve(conn: &Conn, watch: &Arc<Mutex<Watch>>, link: u64) -> String {
        let mut session = Session { moves: Vec::new(), stats_since: Instant::now(), burst: None, paused_noted: false };
        let mut pending: Vec<u8> = Vec::new();
        let mut reader = match Reader::new(conn) {
            Ok(r) => r,
            Err(e) => return e,
        };
        loop {
            let chunk = match reader.next() {
                Ok(Some(chunk)) => chunk,
                Ok(None) => return "对方关闭了连接".into(),
                Err(e) => return e,
            };
            crate::trace::event("rx", &[chunk.len() as f64]);
            pending.extend_from_slice(&chunk);
            while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = pending.drain(..=pos).collect();
                let now = Instant::now();
                watch.lock().unwrap().last_rx = now;
                *LAST_RX.lock().unwrap() = Some(now);
                if !session.handle(&line[..line.len() - 1], watch, conn, link) {
                    return "另一部手机正连着，拒绝了这部".into();
                }
            }
            if pending.len() > 1 << 20 {
                pending.clear(); // 异常数据，丢掉，别撑爆内存
            }
        }
    }

    /// 处理一条消息；返回 false = 这条连接该断开了（别的手机）
    fn handle(&mut self, line: &[u8], watch: &Arc<Mutex<Watch>>, conn: &Conn, link: u64) -> bool {
        let Ok(msg) = serde_json::from_slice::<Value>(line) else {
            log!("⚠ 看不懂的消息：{}", String::from_utf8_lossy(line));
            return true;
        };
        let num = |k: &str| msg.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        let text = |k: &str| msg.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        if crate::trace::dry_run() && matches!(text("t").as_str(), "button" | "hold" | "key" | "text" | "layout" | "clip") {
            crate::trace::event("skip", &[]);
            return true;
        }
        match text("t").as_str() {
            "hello" => {
                // 新版 App：device 是手机的蓝牙名（少爷的mi13），model 是型号；旧版 App 的 device 是「厂商 型号」
                let device = text("device");
                let model = text("model");
                let app = text("app");
                // 蓝牙连上来的手机扫码绑定过的话，hello 里带着它的编号：和局域网连着的不是同一部就拒掉
                let lan_id = text("lan");
                if !lan_id.is_empty() {
                    let mut links = LINKS.lock().unwrap();
                    if other_phone(&links, &lan_id, link) {
                        drop(links);
                        busy(conn);
                        log!("🚫 另一部手机正用局域网连着，拒绝了 {device} 的蓝牙连接");
                        return false;
                    }
                    if let Some(l) = links.iter_mut().find(|l| l.id == link) {
                        l.phone = Some(lan_id);
                    }
                }
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
                reply(conn, &json!({"t": "hello", "host": crate::util::host_name(), "app": env!("CARGO_PKG_VERSION"), "pc": crate::lan::pc_id()}));
                push_config_on(Some(conn));
                if let Some(info) = crate::lan::info() {
                    reply(conn, &info);
                }
            }
            "ping" => {
                reply(conn, &json!({"t": "pong", "id": msg.get("id"), "ts": msg.get("ts")}));
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
            "layout" => save_layout(&msg, conn),
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
                    release_everything();
                }
                log!("{}", if on { "🔆 手机亮屏" } else { "🌙 手机息屏（连接保持）" });
            }
            "release" => release_everything(),
            "clip" => crate::clipsync::from_phone(&text("s")),
            "smooth" => {} // 旧版 App 的开关，现在以电脑端设置为准
            "mem" => {
                let (working_set, private) = crate::util::memory_bytes();
                reply(conn, &json!({"t": "mem", "ws": working_set, "private": private}));
            }
            other => log!("⚠ 不认识的消息类型：{other}"),
        }
        true
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
        {
            let mut holds = HOLDS.lock().unwrap();
            let holds = holds.get_or_insert_with(HashMap::new);
            match holds.get(id) {
                // 中间全部松开过（息屏、看门狗）：这条记录早就作废了，重新按
                Some((_, generation)) if *generation != input::hold_generation() => {
                    holds.remove(id);
                }
                Some(_) => return, // 重复的按下（不该有），别多按一次
                None => {}
            }
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
        HOLDS.lock().unwrap().get_or_insert_with(HashMap::new).insert(id.to_string(), (vks, generation));
        log!("⏬ {} · 按住 {}", module.title(), keys.join("+"));
        state::record_button(id, module.title(), format!("直通 → {}", keys.join("+")), None);
    }

    fn hold_up(&mut self, id: &str) {
        let held = HOLDS.lock().unwrap().get_or_insert_with(HashMap::new).remove(id);
        if let Some((vks, generation)) = held {
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

/// 告诉这条连接：电脑正连着别的手机（手机看到就隔久一点再试），然后断开
fn busy(conn: &Conn) {
    reply(conn, &json!({"t": "busy"}));
    std::thread::sleep(Duration::from_millis(200));
    conn.close();
}

/// 局域网接进来、认证过的连接：在当前线程跑会话，断开才返回
pub fn run_lan(conn: suanniao_lan::SecureConn, phone: String) {
    Session::run(Conn::Lan { conn, phone });
}

/// 从连接上一块一块地读
enum Reader {
    Bt { input: windows::Storage::Streams::IInputStream, buffer: Buffer },
    Lan { conn: suanniao_lan::SecureConn, buffer: Vec<u8> },
}

impl Reader {
    fn new(conn: &Conn) -> std::result::Result<Reader, String> {
        match conn {
            Conn::Bt(socket) => Ok(Reader::Bt {
                input: socket.InputStream().map_err(|e| e.message().to_string())?,
                buffer: Buffer::Create(4096).map_err(|e| e.message().to_string())?,
            }),
            Conn::Lan { conn, .. } => Ok(Reader::Lan { conn: conn.clone(), buffer: vec![0u8; 16384] }),
        }
    }

    /// 读到一块数据；None = 对方关了
    fn next(&mut self) -> std::result::Result<Option<Vec<u8>>, String> {
        match self {
            Reader::Bt { input, buffer } => (|| -> Result<Option<Vec<u8>>> {
                let data = input.ReadAsync(&*buffer, 4096, InputStreamOptions::Partial)?.join()?;
                let len = data.Length()? as usize;
                if len == 0 {
                    return Ok(None);
                }
                let mut chunk = vec![0u8; len];
                DataReader::FromBuffer(&data)?.ReadBytes(&mut chunk)?;
                Ok(Some(chunk))
            })()
            .map_err(|e| e.message().to_string()),
            Reader::Lan { conn, buffer } => match conn.read(buffer) {
                Ok(0) => Ok(None),
                Ok(n) => Ok(Some(buffer[..n].to_vec())),
                Err(e) => Err(e.to_string()),
            },
        }
    }
}

/// 手机上调好的布局：校验后写回配置文件，先把新配置推给手机，再告诉它存好了没有。
fn save_layout(msg: &Value, conn: &Conn) {
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
    let saved = json!({"t": "layoutSaved", "ok": error.is_empty(), "error": error});
    if !reply(conn, &saved) {
        send_to_phone(&saved);
    }
}

/// 亮屏时手机每秒在每条连接上发一次心跳：局域网这么久没收到就当断了（手机关了 Wi-Fi、走远了，
/// 断开的消息发不过来，不主动断的话会一直以为连着，消息都发进死连接）
const LAN_SILENT_LIMIT: Duration = Duration::from_secs(5);

fn spawn_watchdog(watch: Arc<Mutex<Watch>>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        let state = watch.lock().unwrap();
        if !state.alive {
            return;
        }
        if !state.screen_on {
            continue;
        }
        if state.conn.kind() == Kind::Lan && state.last_rx.elapsed() > LAN_SILENT_LIMIT {
            log!("⏱ 局域网 {} 秒没收到手机消息，断开这条连接", LAN_SILENT_LIMIT.as_secs());
            state.conn.close();
            return;
        }
        let quiet = LAST_RX.lock().unwrap().is_none_or(|t| t.elapsed() > Duration::from_secs(10));
        if quiet && input::any_held() {
            release_everything();
            log!("⏱ 10 秒没收到手机消息，已松开按着的鼠标键和键盘键");
        }
    });
}
