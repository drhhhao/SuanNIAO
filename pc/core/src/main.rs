//! 蒜鸟电脑端后台（SuanNIAO-core.exe）：托盘常驻，开蓝牙串口服务（扫码绑定过手机的话还开局域网端口）等手机连接，执行按键映射和触控板、语音文字。
//!
//! 配置由主界面（SuanNIAO.exe）编辑、写到 `%APPDATA%\SuanNIAO\config.json`；这里每半秒看一次文件有没有变，
//! 变了就重新读取并同步给手机。手机上调好的布局由这里校验后写回这个文件。
//! 运行状态写到 status.json，给主界面首页显示。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod btpair;
mod clipboard;
mod clipimage;
mod clipsync;
mod filerecv;
mod input;
mod lan;
mod launcher;
mod link;
mod overlay;
mod playout;
mod state;
mod trace;
mod tray;
mod update;
mod util;

use std::time::{Duration, SystemTime};

use suanniao_config as config;
use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

fn main() {
    // 只允许一个后台在跑
    // 改名前（0.7.4 以前）占的是 Local\PhonePad.Core，主界面要换掉旧后台时两个名字都看
    let _single = unsafe { CreateMutexW(None, true, w!("Local\\SuanNIAO.Core")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return;
    }
    // 没开过新版主界面就先开了后台（比如开机自启）时，在这里搬改名前的数据文件夹
    let moved = config::move_legacy_data_dir();
    util::init_log();
    if let Some(how) = moved {
        log!("📦 改名前的数据文件夹 %APPDATA%\\PhonePad {how} %APPDATA%\\SuanNIAO");
    }
    config::set_local_offset_secs(util::local_offset_secs());
    input::enable_dpi_awareness();
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    state::restore_usage();
    state::load_config_at_start();
    log!("蒜鸟后台 {} 启动（{}），{}", env!("CARGO_PKG_VERSION"), util::host_name(), util::memory());

    // 蓝牙串口服务：电脑蓝牙关着时开不起来，每 10 秒再试一次（打开蓝牙以后不用重启后台）
    std::thread::spawn(|| {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        let mut logged = String::new();
        loop {
            match link::Server::start() {
                Ok(server) => {
                    log!("✅ 蓝牙串口服务已就绪，等手机连接");
                    state::update_status(|s| s.bluetooth_error.clear());
                    // 一直留着：服务随后台退出
                    std::mem::forget(server);
                    return;
                }
                Err(e) => {
                    let message = e.message().to_string();
                    if message != logged {
                        log!("❌ 蓝牙服务启动失败：{message}（确认电脑蓝牙已打开），10 秒后再试");
                        logged = message.clone();
                    }
                    state::update_status(|s| s.bluetooth_error = if message.is_empty() { "蓝牙服务开不起来".into() } else { message });
                }
            }
            std::thread::sleep(Duration::from_secs(10));
        }
    });
    state::update_status(|_| {});
    lan::start();
    std::thread::spawn(watch_config);
    std::thread::spawn(update::run);
    std::thread::spawn(clipsync::run);
    // 状态文件：连发时攒着的改动每 200 毫秒写一次；平时每 5 秒刷新一次时间戳，主界面靠它判断后台还活着
    std::thread::spawn(|| {
        for tick in 1u64.. {
            std::thread::sleep(Duration::from_millis(200));
            if tick % 25 == 0 {
                state::update_status(|_| {});
            } else {
                state::flush_status();
            }
        }
    });

    tray::run(); // 托盘消息循环，选「退出」后返回
    actions::cancel();
    input::release_all();
    clipboard::give_back_now(); // 刚粘贴完就退出：剪贴板原来的内容现在就放回去
    state::mark_stopped();
    log!("蒜鸟后台退出");
}

/// 主界面改完配置会重写文件；看到修改时间变了就重新读取并推给手机。
/// 手机上调布局时是后台自己写的文件（已经推给手机了），认出来就跳过。
fn watch_config() {
    let modified = || std::fs::metadata(config::config_path()).and_then(|m| m.modified()).ok();
    let mut last: Option<SystemTime> = modified();
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let now = modified();
        if now != last {
            last = now;
            if state::is_own_write(now) {
                continue;
            }
            // 读不出来：留着上一份好的，不推给手机
            if !state::reload_config() {
                continue;
            }
            // 改了配置：还没执行的动作不做了，直通按着的键都松开（和 axonkey 一样）
            let dropped = actions::cancel();
            input::release_keys_all();
            link::push_config();
            log!("🔄 配置已更新并同步到手机{}", if dropped > 0 { format!("（取消了 {dropped} 步还没执行的动作）") } else { String::new() });
        }
    }
}
