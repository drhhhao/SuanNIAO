//! 打安装包（发版时 release.yml 用）：suanniao-pack <启动程序> <输出的安装包> <要装的文件>...
//! 把文件压缩后接在启动程序后面（格式见 lib.rs），打完再解开核对一遍。

use std::path::PathBuf;

use suanniao_setup::{build, open, Entry};

fn main() {
    if let Err(e) = run() {
        eprintln!("❌ {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if args.len() < 3 {
        return Err("用法：suanniao-pack <启动程序> <输出的安装包> <要装的文件>...".into());
    }
    let read = |path: &PathBuf| std::fs::read(path).map_err(|e| format!("读不了 {}：{e}", path.display()));
    let stub = read(&args[0])?;
    let mut entries = Vec::new();
    for path in &args[2..] {
        let name = path.file_name().and_then(|name| name.to_str()).ok_or_else(|| format!("文件名不对：{}", path.display()))?;
        entries.push(Entry { name: name.into(), data: read(path)? });
    }
    let installer = build(&stub, &entries)?;
    let back = open(&installer)?;
    if back.len() != entries.len() || back.iter().zip(&entries).any(|(a, b)| a.name != b.name || a.data != b.data) {
        return Err("打完解开对不上".into());
    }
    let out = &args[1];
    if let Some(dir) = out.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("建不了 {}：{e}", dir.display()))?;
    }
    std::fs::write(out, &installer).map_err(|e| format!("写不了 {}：{e}", out.display()))?;
    let mb = |bytes: usize| bytes as f64 / 1024.0 / 1024.0;
    let raw: usize = entries.iter().map(|entry| entry.data.len()).sum();
    println!("✅ {}：{} 个文件 {:.1} MB，安装包 {:.1} MB", out.display(), entries.len(), mb(raw), mb(installer.len()));
    Ok(())
}
