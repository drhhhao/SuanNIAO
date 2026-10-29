//! 蒜鸟电脑端安装包的启动程序。发版时 suanniao-pack 在它后面接上 SuanNIAO.exe 和 SuanNIAO-core.exe，
//! 就是 SuanNIAO-PC-版本-Setup.exe。
//!
//! 双击后把两个 exe 解到临时文件夹，运行里面的 `SuanNIAO.exe --setup`：安装界面就是主界面程序的安装模式，
//! 和蒜鸟长得一样（见 ui/src/setup.rs）。等它结束，删掉临时文件夹。
//! 命令行参数原样传过去（--update、--dir、--silent、--no-desktop）。

#![windows_subsystem = "windows"]

use std::ffi::OsString;
use std::time::Duration;

use windows::core::{w, HSTRING};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let code = match run(&args) {
        Ok(code) => code,
        Err(message) => {
            if !args.iter().any(|arg| arg == "--silent") {
                unsafe {
                    MessageBoxW(None, &HSTRING::from(message), w!("蒜鸟安装"), MB_OK | MB_ICONERROR);
                }
            }
            1
        }
    };
    std::process::exit(code);
}

fn run(args: &[OsString]) -> Result<i32, String> {
    let exe = std::env::current_exe().map_err(|e| format!("找不到安装包：{e}"))?;
    let entries = suanniao_setup::open(&std::fs::read(&exe).map_err(|e| format!("读不了安装包：{e}"))?)?;
    let temp = std::env::temp_dir();
    let dir = temp.join(format!("SuanNIAO-Setup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("建不了临时文件夹：{e}"))?;
    let unpacked = entries.iter().try_for_each(|entry| std::fs::write(dir.join(&entry.name), &entry.data));
    drop(entries);
    let result = match unpacked {
        Ok(()) => std::process::Command::new(dir.join("SuanNIAO.exe"))
            .arg("--setup")
            .args(args)
            .current_dir(&temp)
            .status()
            .map(|status| status.code().unwrap_or(1))
            .map_err(|e| format!("安装程序打不开：{e}")),
        Err(e) => Err(format!("解不开安装文件：{e}")),
    };
    // 安装界面刚退出时 exe 可能还被占着一小会儿，多试几次
    for _ in 0..40 {
        if std::fs::remove_dir_all(&dir).is_ok() || !dir.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    result
}
