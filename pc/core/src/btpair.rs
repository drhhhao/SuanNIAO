//! 扫码蓝牙配对：二维码开着时，二维码里还带着这台电脑的蓝牙地址（bt），一次性配对码（c）兼当口令。
//! 手机扫到以后直接对这个地址发起配对，手机上点一下系统弹的「配对」就行；电脑这边在二维码开着期间
//! 接管 Windows 的配对确认、自动同意，不弹「是否配对」。
//! 配对好手机用蓝牙连上来，hello 里带着口令（btc）：对得上就算配对成功（主界面关掉二维码、提示一声）；
//! 对不上，或者配对后 2 分钟没带口令连上来，就删掉这次配对——防止旁边的人趁二维码开着配进来。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::Devices::Bluetooth::BluetoothAdapter;
use windows::Win32::Devices::Bluetooth::{
    BluetoothRegisterForAuthenticationEx, BluetoothRemoveDevice, BluetoothSendAuthenticationResponseEx, BluetoothUnregisterAuthentication,
    BLUETOOTH_ADDRESS, BLUETOOTH_AUTHENTICATE_RESPONSE, BLUETOOTH_AUTHENTICATION_CALLBACK_PARAMS,
    BLUETOOTH_AUTHENTICATION_METHOD_NUMERIC_COMPARISON, BLUETOOTH_AUTHENTICATION_METHOD_PASSKEY_NOTIFICATION,
};

use suanniao_config as config;

use crate::log;

/// 配对后这么久还没带口令连上来，就删掉这次配对
const VERIFY_SECS: u64 = 120;

#[derive(Default)]
struct State {
    /// 二维码开着
    open: bool,
    /// 接管配对确认的登记（二维码开着时有；登记不上就由 Windows 自己弹确认）
    reg: Option<isize>,
    /// 这台电脑的蓝牙地址（AA:BB:CC:DD:EE:FF），二维码开着时读一次
    addr: String,
    /// 口令和过期时刻（Unix 秒）：最新的在最后，留着上一个（二维码换新时正在配对的手机拿的是旧的）
    codes: Vec<(String, u64)>,
    /// 经我们同意配对上的手机地址和时刻，等它带口令连上来
    pending: Vec<(u64, Instant, String)>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut guard = STATE.lock().unwrap();
    f(guard.get_or_insert_with(State::default))
}

/// 这台电脑的蓝牙地址；没有蓝牙（或者不支持经典蓝牙）是 None
fn adapter_address() -> Option<String> {
    let adapter = BluetoothAdapter::GetDefaultAsync().ok()?.join().ok()?;
    if !adapter.IsClassicSupported().unwrap_or(false) {
        return None;
    }
    let a = adapter.BluetoothAddress().ok()?;
    (a != 0).then(|| format_addr(a))
}

fn format_addr(a: u64) -> String {
    (0..6).rev().map(|i| format!("{:02X}", (a >> (i * 8)) & 0xff)).collect::<Vec<_>>().join(":")
}

/// 「(c0:3c:59:12:34:56)」「C0:3C:59:12:34:56」这样的地址读成数字
pub fn parse_addr(text: &str) -> Option<u64> {
    let hex: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 12 {
        return None;
    }
    u64::from_str_radix(&hex, 16).ok()
}

/// 二维码打开、换了新的配对码：它就是新口令，开始接管配对确认
pub fn open(code: &str) {
    let addr = adapter_address();
    with(|s| {
        let now = config::now_secs();
        s.codes.retain(|(_, exp)| *exp > now);
        s.codes.push((code.to_string(), now + 600));
        if s.codes.len() > 2 {
            s.codes.remove(0);
        }
        s.addr = addr.unwrap_or_default();
        s.open = true;
        if s.reg.is_none() && !s.addr.is_empty() {
            let mut handle = 0isize;
            let rc = unsafe { BluetoothRegisterForAuthenticationEx(None, &mut handle, Some(on_auth), None) };
            if rc == 0 {
                s.reg = Some(handle);
                log!("🔵 二维码开着：手机扫码发起的蓝牙配对会自动同意（这台电脑 {}）", s.addr);
            } else {
                log!("⚠ 接管不了蓝牙配对确认（错误 {rc}），手机扫码配对时电脑上会弹 Windows 的确认");
            }
        }
    });
}

/// 二维码关了：不再自动同意配对（口令留到过期，正在配对的手机还能用它连上来）
pub fn close() {
    with(|s| {
        s.open = false;
        if let Some(handle) = s.reg.take() {
            let _ = unsafe { BluetoothUnregisterAuthentication(handle) };
        }
    });
}

/// 放进二维码的蓝牙地址；二维码没开、没有蓝牙是空的
pub fn qr_addr() -> String {
    with(|s| if s.open { s.addr.clone() } else { String::new() })
}

/// 蓝牙连上来的手机打招呼了。返回 Some(true)：口令对，扫码配对成功；Some(false)：刚配上却拿不出对的口令，
/// 已经删掉配对，该断开；None：和扫码配对无关（早就配对过、没带口令）
pub fn hello(peer: &str, code: &str) -> Option<bool> {
    let addr = parse_addr(peer)?;
    let now = config::now_secs();
    let (ok, pending) = with(|s| {
        let ok = !code.is_empty() && s.codes.iter().any(|(c, exp)| c == code && *exp > now);
        let pending = s.pending.iter().any(|(a, _, _)| *a == addr);
        s.pending.retain(|(a, _, _)| *a != addr);
        (ok, pending)
    });
    if ok {
        return Some(true);
    }
    if pending {
        remove(addr, "连上来时没带对的口令");
        return Some(false);
    }
    None
}

/// 每隔几秒调一次：同意配对后 2 分钟还没带口令连上来的，删掉
pub fn sweep() {
    let expired: Vec<(u64, String)> = with(|s| {
        let (old, keep): (Vec<_>, Vec<_>) =
            s.pending.drain(..).partition(|(_, at, _)| at.elapsed() > Duration::from_secs(VERIFY_SECS));
        s.pending = keep;
        old.into_iter().map(|(a, _, name)| (a, name)).collect()
    });
    for (addr, name) in expired {
        remove(addr, &format!("「{name}」配对后 {VERIFY_SECS} 秒没用蒜鸟连上来"));
    }
}

fn remove(addr: u64, why: &str) {
    let address = BLUETOOTH_ADDRESS { Anonymous: windows::Win32::Devices::Bluetooth::BLUETOOTH_ADDRESS_0 { ullLong: addr } };
    let rc = unsafe { BluetoothRemoveDevice(&address) };
    log!("✂ 删掉了蓝牙配对 {}：{why}{}", format_addr(addr), if rc == 0 { String::new() } else { format!("（没删成，错误 {rc}）") });
}

/// Windows 问「要不要和这个设备配对」：二维码开着时替用户点「是」，记下来等它带口令连上来
unsafe extern "system" fn on_auth(
    _param: *const core::ffi::c_void,
    params: *const BLUETOOTH_AUTHENTICATION_CALLBACK_PARAMS,
) -> windows::core::BOOL {
    let Some(p) = (unsafe { params.as_ref() }) else { return true.into() };
    let addr = unsafe { p.deviceInfo.Address.Anonymous.ullLong };
    let name_len = p.deviceInfo.szName.iter().position(|&c| c == 0).unwrap_or(p.deviceInfo.szName.len());
    let name = String::from_utf16_lossy(&p.deviceInfo.szName[..name_len]);
    let method = p.authenticationMethod;
    let mut response = BLUETOOTH_AUTHENTICATE_RESPONSE {
        bthAddressRemote: p.deviceInfo.Address,
        authMethod: method,
        ..Default::default()
    };
    if method == BLUETOOTH_AUTHENTICATION_METHOD_NUMERIC_COMPARISON {
        response.Anonymous.numericCompInfo.NumericValue = unsafe { p.Anonymous.Numeric_Value };
    } else if method == BLUETOOTH_AUTHENTICATION_METHOD_PASSKEY_NOTIFICATION {
        response.Anonymous.passkeyInfo.passkey = unsafe { p.Anonymous.Passkey };
    } else {
        // 老式 PIN 码之类：替用户填不了，拒掉（手机上会说配对失败）
        response.negativeResponse = 1;
        log!("⚠ 「{name}」要用老式 PIN 码配对，自动配对不支持，拒绝了");
    }
    let rc = unsafe { BluetoothSendAuthenticationResponseEx(None, &response) };
    if response.negativeResponse == 0 {
        if rc == 0 {
            log!("🔵 自动同意了「{name}」（{}）的蓝牙配对，等它用蒜鸟连上来核对口令", format_addr(addr));
            with(|s| {
                s.pending.retain(|(a, _, _)| *a != addr);
                s.pending.push((addr, Instant::now(), name));
            });
        } else {
            log!("⚠ 同意「{name}」的蓝牙配对没成功：错误 {rc}");
        }
    }
    true.into()
}
