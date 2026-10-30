//! 局域网连接的界面：设置页里绑定过的手机、扫码绑定的二维码弹窗、首页显示现在走哪种连接。
//!
//! 二维码、配对码都是后台（SuanNIAO-core.exe）出的：界面写 lan-command.json 说「二维码开着」，
//! 后台生成一次性配对码写进 status.json 的 lan，界面每秒读状态时把它画成二维码。
//! 手机扫码绑定上以后，后台在状态里记下 justPaired，界面看到就关掉二维码、提示一声。

use slint::{Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use suanniao_config::{self as config, LanCommand, Status};

use crate::{AppWindow, LanPhoneRow};

#[derive(Default)]
pub struct LanUi {
    /// 二维码开着，打开的时刻（Unix 秒）：这之后的 justPaired 才算这次扫的
    open_at: Option<u64>,
    /// 画着的二维码里的内容，没变就不重画
    payload: String,
    /// 设置页的手机列表，没变就不换模型
    phones: Vec<LanPhoneRow>,
    /// 点了「解除绑定」、后台还没删掉的手机：每次写指令都带上，直到状态里没有它们了
    removing: Vec<String>,
    /// 上次写指令的时刻（二维码开着时定时重写，后台靠它知道界面还开着）
    sent_at: u64,
}

/// 二维码里的内容：手机扫到以后认这个格式
/// `suanniao://pair?v=1&pc=电脑编号&n=电脑名&a=地址1,地址2&p=端口&fp=证书指纹&c=配对码&bt=蓝牙地址`
/// 手机按扫码时选的连接方式用：局域网连 a、p，蓝牙对 bt 发起配对（c 兼当口令）。
/// 没连网时 a 是空的、只能蓝牙配对；电脑没有蓝牙时没有 bt
fn payload(status: &Status) -> Option<String> {
    let lan = &status.lan;
    if lan.pair_code.is_empty() || lan.port == 0 || lan.fingerprint.is_empty() || (lan.addrs.is_empty() && lan.bt_addr.is_empty()) {
        return None;
    }
    let host = std::env::var("COMPUTERNAME").unwrap_or_default();
    let mut text = format!(
        "suanniao://pair?v=1&pc={}&n={}&a={}&p={}&fp={}&c={}",
        encode(&lan.pc_id),
        encode(host.trim()),
        encode(&lan.addrs.join(",")),
        lan.port,
        encode(&lan.fingerprint),
        encode(&lan.pair_code)
    );
    if !lan.bt_addr.is_empty() {
        text.push_str(&format!("&bt={}", encode(&lan.bt_addr)));
    }
    Some(text)
}

/// 网址里的百分号编码（字母数字和 -._~ 原样，其余按 UTF-8 字节编码）
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~,:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// 画二维码：黑块白底，四周留 2 格白边，每格整数个像素
fn qr_image(text: &str) -> Option<Image> {
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    let quiet = 2;
    let cells = width + quiet * 2;
    let scale = (480 / cells).max(2);
    let side = (cells * scale) as u32;
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(side, side);
    let pixels = buffer.make_mut_slice();
    for (i, pixel) in pixels.iter_mut().enumerate() {
        let (x, y) = (i % side as usize / scale, i / side as usize / scale);
        let dark = x >= quiet
            && y >= quiet
            && x < quiet + width
            && y < quiet + width
            && colors[(y - quiet) * width + (x - quiet)] == qrcode::Color::Dark;
        let v = if dark { 0 } else { 255 };
        *pixel = Rgba8Pixel { r: v, g: v, b: v, a: 255 };
    }
    Some(Image::from_rgba8(buffer))
}

fn send(pairing: bool, remove: &[String]) {
    let seq = config::read_lan_command().map(|c| c.seq).unwrap_or(0) + 1;
    let _ = config::write_lan_command(&LanCommand { seq, pairing, remove: remove.to_vec(), at: config::now_secs() });
}

fn ago(secs: u64) -> String {
    let now = config::now_secs();
    let d = now.saturating_sub(secs);
    match d {
        _ if secs == 0 => "没连过".into(),
        0..=59 => "刚刚".into(),
        60..=3599 => format!("{} 分钟前", d / 60),
        3600..=86399 => format!("{} 小时前", d / 3600),
        _ => format!("{} 天前", d / 86400),
    }
}

impl LanUi {
    /// 「显示二维码」
    pub fn open(&mut self, ui: &AppWindow) {
        self.open_at = Some(config::now_secs());
        self.payload.clear();
        ui.set_lan_qr_ready(false);
        ui.set_lan_addr_text("".into());
        ui.set_lan_qr_note("".into());
        ui.set_lan_open(true);
        self.send();
    }

    /// 关掉二维码：配对码随之作废
    pub fn close(&mut self, ui: &AppWindow) {
        if self.open_at.take().is_some() {
            self.send();
        }
        ui.set_lan_open(false);
    }

    pub fn remove(&mut self, id: &str) {
        if !self.removing.iter().any(|r| r == id) {
            self.removing.push(id.to_string());
        }
        self.send();
    }

    fn send(&mut self) {
        self.sent_at = config::now_secs();
        send(self.open_at.is_some(), &self.removing);
    }

    /// 每秒读完状态调一次；扫码绑定成功时返回提示语
    pub fn refresh(&mut self, ui: &AppWindow, running: bool, status: &Status) -> Option<String> {
        let lan = &status.lan;
        // 后台删掉了的就不用再说了（后台没运行时状态是旧的，先留着）
        if running && !self.removing.is_empty() {
            self.removing.retain(|id| lan.phones.iter().any(|p| &p.id == id));
        }
        let links: Vec<&str> = status.links.iter().map(|k| if k == "lan" { "局域网" } else { "蓝牙" }).collect();
        ui.set_link_kind(if status.connected && running { links.join(" + ").into() } else { "".into() });
        ui.set_lan_online(status.connected && running && status.links.iter().any(|k| k == "lan"));
        ui.set_bt_online(status.connected && running && status.links.iter().any(|k| k == "bt"));
        ui.set_lan_error(if running { lan.error.as_str().into() } else { "".into() });

        let phones: Vec<LanPhoneRow> = lan
            .phones
            .iter()
            .filter(|p| !self.removing.contains(&p.id)) // 点了就先从列表里拿掉，不等后台
            .map(|p| LanPhoneRow {
                id: p.id.as_str().into(),
                name: if p.name.is_empty() { "手机".into() } else { p.name.as_str().into() },
                detail: if p.online && running {
                    format!("{} · 正用局域网连着", p.model).into()
                } else {
                    format!("{} · 局域网 · 上次连接 {}", p.model, ago(p.last_seen.max(p.paired_at))).into()
                },
                online: p.online,
            })
            .collect();
        if phones != self.phones {
            self.phones = phones.clone();
            ui.set_lan_phones(ModelRc::new(VecModel::from(phones)));
        }

        let open_at = self.open_at?;
        if !running {
            ui.set_lan_qr_ready(false);
            ui.set_lan_addr_text("".into());
            ui.set_lan_qr_note("蒜鸟后台没在运行，二维码出不来。".into());
            return None;
        }
        if !lan.just_paired.is_empty() && lan.just_paired_at >= open_at {
            self.close(ui);
            return Some(if lan.just_paired_via == "bt" {
                format!("已用蓝牙配对「{}」，以后会自动连上", lan.just_paired)
            } else {
                format!("已绑定「{}」，同一个网络里会自动连上", lan.just_paired)
            });
        }
        // 后台刚启动、没收到这次的指令（或者没处理）：过 3 秒再说一次；开着就每 30 秒重写一次，后台知道界面还开着
        let now = config::now_secs();
        let resend = if lan.pair_code.is_empty() { 3 } else { 30 };
        if now >= open_at + 3 && now >= self.sent_at + resend {
            self.send();
        }
        match payload(status) {
            Some(text) => {
                if text != self.payload {
                    match qr_image(&text) {
                        Some(image) => {
                            ui.set_lan_qr(image);
                            ui.set_lan_qr_ready(true);
                        }
                        None => ui.set_lan_qr_ready(false),
                    }
                    self.payload = text;
                }
                let bt = !lan.bt_addr.is_empty();
                ui.set_lan_addr_text(
                    match (bt, lan.addrs.is_empty()) {
                        (true, true) => "这台电脑没连网：只能用蓝牙配对".to_string(),
                        (true, false) => format!("蓝牙 · 局域网 {} · 端口 {}", lan.addrs.join("、"), lan.port),
                        _ => format!("局域网 {} · 端口 {}", lan.addrs.join("、"), lan.port),
                    }
                    .into(),
                );
                let left = lan.pair_expires.saturating_sub(config::now_secs()).div_ceil(60).max(1);
                ui.set_lan_qr_note(
                    if bt {
                        format!(
                            "蓝牙：手机上点「配对」就好，电脑自动同意。局域网：两边要连同一个网络。{left} 分钟内有效，会自动换新的。"
                        )
                    } else {
                        format!(
                            "两边要连同一个网络。二维码只能用一次，{left} 分钟内有效。连不上：看看防火墙，或 Wi-Fi 是否开了「设备隔离」（访客网络常这样）。"
                        )
                    }
                    .into(),
                );
            }
            None => {
                ui.set_lan_qr_ready(false);
                if !lan.error.is_empty() {
                    ui.set_lan_addr_text("".into());
                    ui.set_lan_qr_note(lan.error.as_str().into());
                } else if lan.listening && lan.addrs.is_empty() {
                    ui.set_lan_addr_text("".into());
                    ui.set_lan_qr_note("这台电脑没连网，连上后二维码会自动出来。".into());
                }
            }
        }
        None
    }
}
