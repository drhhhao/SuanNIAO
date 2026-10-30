//! 局域网连接：手机扫电脑上的二维码绑定，以后在同一个网络里（能互相连通就行，不一定同一网段）自动走 TCP 连上。
//! 加密和认证在 suanniao-lan 里（TLS + 手机钉住证书指纹 + 一次性配对码 / 长期令牌），这里管：
//! - 什么时候开始听：绑定过手机，或者主界面打开了二维码（没用过局域网的电脑不开端口，也不会弹防火墙提示）
//! - 主界面的指令（lan-command.json）：要配对码、作废配对码、解除绑定
//! - 这台电脑的地址（有线、无线网卡的 IPv4），变了告诉连着的手机
//! - 状态写进 status.json 的 lan，主界面据此画二维码、列已绑定的手机
//!
//! 认证过的连接交给 link::run_lan，和蓝牙连接跑一样的会话。

use std::net::{Ipv4Addr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::json;
use suanniao_config::{self as config, LanPhone};
use suanniao_lan::{Identity, Pairings, Server};

use crate::{link, log, state};

/// 默认端口；被占了就让系统挑一个，记在 lan\port 里，下次还用它（手机记着的是上次的端口）
const DEFAULT_PORT: u16 = 47820;
/// 配对码 10 分钟有效；二维码开着时剩不到 1 分钟就换一个新的
const PAIR_SECS: u64 = 600;
const PAIR_RENEW_SECS: u64 = 60;

struct Lan {
    identity: Identity,
    pairings: Arc<Pairings>,
    server: Server,
    listening: AtomicBool,
    port: Mutex<u16>,
    addrs: Mutex<Vec<String>>,
    /// 现在的配对码和过期时刻（Unix 秒）
    pair: Mutex<Option<(String, u64)>>,
    /// 刚扫码绑定上的手机名、时刻、走的哪种连接（"lan" / "bt"）
    just_paired: Mutex<(String, u64, &'static str)>,
}

static LAN: OnceLock<Lan> = OnceLock::new();

fn dir() -> PathBuf {
    config::lan_dir()
}

/// 后台启动时调：读证书和绑定过的手机，有绑定过的手机就开始听；之后看主界面的指令、网卡地址的变化
pub fn start() {
    std::thread::spawn(|| {
        let loaded = (|| -> std::io::Result<Lan> {
            std::fs::create_dir_all(dir())?;
            let identity = Identity::load_or_create(&dir())?;
            let pairings = Arc::new(Pairings::load(&dir())?);
            let server = Server::new(&identity)?;
            Ok(Lan {
                identity,
                pairings,
                server,
                listening: AtomicBool::new(false),
                port: Mutex::new(0),
                addrs: Mutex::new(local_addrs()),
                pair: Mutex::new(None),
                just_paired: Mutex::new((String::new(), 0, "")),
            })
        })();
        let lan = match loaded {
            Ok(lan) => lan,
            Err(e) => {
                log!("❌ 局域网连接开不起来：{e}");
                state::update_status(|s| s.lan.error = format!("读不出局域网证书或绑定记录：{e}"));
                return;
            }
        };
        let _ = LAN.set(lan);
        let lan = LAN.get().unwrap();
        if !lan.pairings.phones().is_empty() {
            listen(lan);
        }
        refresh_status();
        watch(lan);
    });
}

/// 开始听（已经在听就什么都不做）
fn listen(lan: &'static Lan) {
    if lan.listening.swap(true, Ordering::SeqCst) {
        return;
    }
    let port_file = dir().join("port");
    let saved = std::fs::read_to_string(&port_file).ok().and_then(|t| t.trim().parse::<u16>().ok()).unwrap_or(DEFAULT_PORT);
    let bound = TcpListener::bind((Ipv4Addr::UNSPECIFIED, saved)).or_else(|_| TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)));
    let listener = match bound {
        Ok(l) => l,
        Err(e) => {
            lan.listening.store(false, Ordering::SeqCst);
            log!("❌ 局域网端口开不起来：{e}");
            state::update_status(|s| s.lan.error = format!("端口开不起来：{e}"));
            return;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(saved);
    if port != saved || !port_file.exists() {
        let _ = std::fs::write(&port_file, port.to_string());
    }
    *lan.port.lock().unwrap() = port;
    log!("✅ 局域网连接已就绪：端口 {port}，地址 {}", lan.addrs.lock().unwrap().join("、"));
    state::update_status(|s| s.lan.error.clear());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(tcp) = stream else { continue };
            let peer = tcp.peer_addr().map(|a| a.ip().to_string()).unwrap_or_else(|_| "?".into());
            std::thread::spawn(move || match lan.server.accept(tcp, &lan.pairings) {
                Ok((conn, phone)) => {
                    if phone.newly_paired {
                        log!("🔗 扫码绑定了手机：{}（{}）", phone.name, phone.model);
                        *lan.pair.lock().unwrap() = None;
                        *lan.just_paired.lock().unwrap() = (phone.name.clone(), config::now_secs(), "lan");
                    }
                    link::run_lan(conn, phone.id);
                }
                Err(e) => {
                    let why = match e.reason() {
                        "badCode" => "二维码过期了或者已经用过",
                        "unknown" => "这部手机已经解除绑定",
                        "badToken" => "手机的凭证不对",
                        "timeout" => "超时",
                        _ => "不是蒜鸟手机端或者数据不对",
                    };
                    log!("🚫 拒绝了来自 {peer} 的局域网连接：{why}（{}）", e.reason());
                }
            });
        }
    });
}

/// 每半秒看一次主界面的指令，每 10 秒看一次网卡地址
fn watch(lan: &'static Lan) {
    let mut handled = config::read_lan_command().map(|c| c.seq).unwrap_or(0);
    // 上次退出时二维码还开着：主界面重开二维码会再写指令，这里不当真；
    // 但没删掉的手机照删（后台没运行时点了「解除绑定」，删几次都一样）
    if let Some(command) = config::read_lan_command() {
        remove_phones(lan, &command.remove);
        refresh_status();
    }
    let mut tick = 0u64;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        tick += 1;
        if let Some(command) = config::read_lan_command() {
            if command.seq != handled {
                handled = command.seq;
                apply(lan, &command);
            }
            // 二维码一直开着：配对码快过期就换新的。界面 90 秒没重写指令（开着二维码时被关掉了）就不续了
            if command.pairing && config::now_secs().saturating_sub(command.at) < 90 {
                let renew = lan.pair.lock().unwrap().as_ref().is_some_and(|(_, exp)| exp.saturating_sub(config::now_secs()) < PAIR_RENEW_SECS);
                if renew {
                    new_code(lan);
                }
            }
        }
        if tick % 10 == 0 {
            crate::btpair::sweep();
        }
        if tick % 20 == 0 {
            let fresh = local_addrs();
            let changed = *lan.addrs.lock().unwrap() != fresh;
            if changed {
                log!("🌐 这台电脑的局域网地址变了：{}", if fresh.is_empty() { "没有可用的网络".to_string() } else { fresh.join("、") });
                *lan.addrs.lock().unwrap() = fresh;
                refresh_status();
                send_info();
            }
        }
    }
}

fn apply(lan: &'static Lan, command: &config::LanCommand) {
    if command.pairing {
        listen(lan);
        let fresh = lan.pair.lock().unwrap().as_ref().is_none_or(|(_, exp)| *exp <= config::now_secs() + PAIR_RENEW_SECS);
        if fresh {
            new_code(lan);
        }
    } else {
        crate::btpair::close();
        if lan.pair.lock().unwrap().take().is_some() {
            lan.pairings.cancel_pair_code();
        }
    }
    remove_phones(lan, &command.remove);
    refresh_status();
}

/// 解除绑定这些手机；已经删掉的跳过（界面每次都把没确认删掉的写上）
fn remove_phones(lan: &Lan, ids: &[String]) {
    let phones = lan.pairings.phones();
    for id in ids {
        let Some(phone) = phones.iter().find(|p| &p.id == id) else { continue };
        match lan.pairings.remove(id) {
            Ok(()) => {
                log!("✂ 解除绑定了手机：{}", phone.name);
                link::drop_lan_phone(id);
            }
            Err(e) => log!("⚠ 解除绑定没成功：{e}"),
        }
    }
}

fn new_code(lan: &Lan) {
    let code = lan.pairings.new_pair_code();
    // 蓝牙扫码配对用同一个码当口令（二维码里的 bc）
    crate::btpair::open(&code);
    *lan.pair.lock().unwrap() = Some((code, config::now_secs() + PAIR_SECS));
    refresh_status();
}

/// 把局域网的情况写进 status.json
pub fn refresh_status() {
    let Some(lan) = LAN.get() else { return };
    let online = link::lan_phone();
    let phones: Vec<LanPhone> = lan
        .pairings
        .phones()
        .into_iter()
        .map(|p| LanPhone {
            online: online.as_deref() == Some(p.id.as_str()),
            id: p.id,
            name: p.name,
            model: p.model,
            paired_at: p.paired_at,
            last_seen: p.last_seen,
        })
        .collect();
    let (code, expires) = lan.pair.lock().unwrap().clone().unwrap_or_default();
    let just = lan.just_paired.lock().unwrap().clone();
    let listening = lan.listening.load(Ordering::SeqCst);
    let port = *lan.port.lock().unwrap();
    let addrs = lan.addrs.lock().unwrap().clone();
    let bt_addr = crate::btpair::qr_addr();
    state::update_status(|s| {
        s.lan.listening = listening;
        s.lan.port = port;
        s.lan.addrs = addrs;
        s.lan.pc_id = lan.identity.pc_id().to_string();
        s.lan.fingerprint = lan.identity.fingerprint();
        s.lan.pair_code = code;
        s.lan.pair_expires = expires;
        s.lan.just_paired = just.0;
        s.lan.just_paired_at = just.1;
        s.lan.just_paired_via = just.2.to_string();
        s.lan.bt_addr = bt_addr;
        s.lan.phones = phones;
    });
}

/// 手机扫码用蓝牙配对上了（btpair 核对过口令）：主界面关掉二维码、提示一声
pub fn bt_paired(name: &str) {
    let Some(lan) = LAN.get() else { return };
    *lan.just_paired.lock().unwrap() = (name.to_string(), config::now_secs(), "bt");
    refresh_status();
}

/// 这部手机还绑定着（解除了、重新扫码换了编号就是 false）
pub fn known_phone(id: &str) -> bool {
    LAN.get().is_some_and(|l| l.pairings.phones().iter().any(|p| p.id == id))
}

/// 这台电脑的局域网编号（还没读出来是空的）
pub fn pc_id() -> String {
    LAN.get().map(|l| l.identity.pc_id().to_string()).unwrap_or_default()
}

/// 告诉连着的手机现在的地址（蓝牙连着也发：手机靠它更新记着的局域网地址，IP 变了也不用重新扫码）
pub fn send_info() {
    if let Some(info) = info() {
        link::send_to_phone(&info);
    }
}

/// 发给手机的地址消息；没在监听是 None
pub fn info() -> Option<serde_json::Value> {
    let lan = LAN.get()?;
    if !lan.listening.load(Ordering::SeqCst) {
        return None;
    }
    Some(json!({
        "t": "lan",
        "pc": lan.identity.pc_id(),
        "addrs": *lan.addrs.lock().unwrap(),
        "port": *lan.port.lock().unwrap(),
        "fp": lan.identity.fingerprint(),
    }))
}

/// 有线、无线网卡上的 IPv4 地址：跳过虚拟网卡（代理软件的 TUN、Hyper-V、VMware、VPN），有网关的排前面
pub fn local_addrs() -> Vec<String> {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
        IP_ADAPTER_ADDRESSES_LH,
    };
    use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};

    const IF_TYPE_ETHERNET: u32 = 6;
    const IF_TYPE_WIFI: u32 = 71;
    let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size: u32 = 32 * 1024;
    let mut buf: Vec<u64> = Vec::new();
    for _ in 0..3 {
        buf = vec![0u64; (size as usize).div_ceil(8)];
        let rc = unsafe { GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH), &mut size) };
        if rc == 0 {
            break;
        }
        if rc != 111 {
            // 111 = ERROR_BUFFER_OVERFLOW：size 已经改成要的大小，再来一次
            return Vec::new();
        }
    }
    let mut found: Vec<(bool, String)> = Vec::new();
    let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    unsafe {
        while !p.is_null() {
            let a = &*p;
            p = a.Next;
            if a.OperStatus != IfOperStatusUp || (a.IfType != IF_TYPE_ETHERNET && a.IfType != IF_TYPE_WIFI) {
                continue;
            }
            let desc = a.Description.to_string().unwrap_or_default().to_lowercase();
            let has_gateway = !a.FirstGatewayAddress.is_null();
            // 用了 Hyper-V「外部虚拟交换机」的电脑，真正的地址在这块虚拟网卡上（有网关）；
            // 没网关的那种是 Default Switch 之类的内部网络，照样跳过
            let external_switch = has_gateway && desc.contains("hyper-v virtual ethernet adapter");
            if !external_switch
                && ["virtual", "vmware", "hyper-v", "tap-", "tap ", "wireguard", "tunnel", "vpn", "loopback", "wintun", "meta"]
                    .iter()
                    .any(|w| desc.contains(w))
            {
                continue;
            }
            let mut u = a.FirstUnicastAddress;
            while !u.is_null() {
                let sa = (*u).Address.lpSockaddr;
                if !sa.is_null() && (*sa).sa_family == AF_INET {
                    let sin = &*(sa as *const SOCKADDR_IN);
                    let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.S_un.S_addr));
                    if !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() {
                        found.push((has_gateway, ip.to_string()));
                    }
                }
                u = (*u).Next;
            }
        }
    }
    found.sort_by_key(|(gw, _)| !*gw);
    let mut out: Vec<String> = Vec::new();
    for (_, ip) in found {
        if !out.contains(&ip) {
            out.push(ip);
        }
    }
    out.truncate(4);
    out
}
