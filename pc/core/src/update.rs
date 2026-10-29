//! 检查新版本（`suanniao-update`）：后台启动 1 分钟后查一次，以后每 6 小时查一次，没查成就半小时后再试；
//! 用户在主界面「关于」里关了「自动检查更新」就不查。
//! 查到新版本、主界面又没开着，就在托盘弹一个通知，一个版本只弹一次（主界面开着时由主界面弹更新窗口）。
//! 托盘菜单里会多一项「更新到 x.y.z…」。点通知或者这一项，都是让主界面打开更新窗口（`SuanNIAO.exe --show-update`），
//! 下载、安装都由主界面做。

use std::time::Duration;

use suanniao_config as config;
use suanniao_update as update;
use windows::core::w;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};

use crate::{log, tray};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const EVERY_SECS: u64 = 6 * 3600;
const RETRY_SECS: u64 = 30 * 60;

pub fn run() {
    std::thread::sleep(Duration::from_secs(60));
    let (mut found, mut failed) = (String::new(), String::new());
    loop {
        let state = update::load();
        let wait = if state.error.is_empty() { EVERY_SECS } else { RETRY_SECS };
        if state.auto && config::now_secs().saturating_sub(state.checked) >= wait {
            match update::check() {
                Ok(manifest) => {
                    failed.clear();
                    if update::newer(&manifest.pc.version, VERSION) && manifest.pc.version != found {
                        log!("🆕 有新版本：电脑端 {}（{}）", manifest.pc.version, manifest.tag);
                        found = manifest.pc.version;
                    }
                }
                Err(e) => {
                    if e != failed {
                        log!("⚠ 检查新版本没成功：{e}");
                        failed = e;
                    }
                }
            }
        }
        notify_once();
        std::thread::sleep(Duration::from_secs(10 * 60));
    }
}

/// 托盘菜单要不要显示「更新到 x…」：有新版本、用户也没跳过它
pub fn offered() -> Option<String> {
    update::load().offered(VERSION).map(|manifest| manifest.pc.version.clone())
}

/// 有新版本、没跳过、没提示过、主界面也没开着：托盘弹通知，记下提示过了
fn notify_once() {
    let state = update::load();
    let Some(manifest) = state.offered(VERSION) else { return };
    if state.notified == manifest.pc.version || ui_running() {
        return;
    }
    let version = manifest.pc.version.clone();
    tray::balloon(&format!("蒜鸟有新版本 {version}"), "点这里看看更新了什么");
    log!("🆕 托盘提示了新版本 {version}");
    update::update(|state| state.notified = version);
}

/// 主界面开着（它占着 Local\SuanNIAO.UI）
fn ui_running() -> bool {
    match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, w!("Local\\SuanNIAO.UI")) } {
        Ok(handle) => {
            unsafe {
                let _ = CloseHandle(handle);
            }
            true
        }
        Err(_) => false,
    }
}
