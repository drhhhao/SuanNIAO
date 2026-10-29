//! 触控板时序记录，排查卡顿用。设了环境变量 PHONEPAD_TRACE（文件路径）才记，一行一个事件：「微秒 类型 数值…」，
//! 微秒从后台启动算起。平时不设，什么都不做。
//!
//! 再设 PHONEPAD_DRYRUN=1：只记录、不真的动鼠标键盘，手机上的按键也不执行。自动化测试时用，免得点到用户的窗口。

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

static START: LazyLock<Instant> = LazyLock::new(Instant::now);
static OUT: LazyLock<Option<Mutex<BufWriter<File>>>> = LazyLock::new(|| {
    let file = File::create(std::env::var_os("PHONEPAD_TRACE")?).ok()?;
    LazyLock::force(&START);
    Some(Mutex::new(BufWriter::new(file)))
});
static DRY_RUN: LazyLock<bool> = LazyLock::new(|| std::env::var_os("PHONEPAD_DRYRUN").is_some_and(|v| v == "1"));

/// 只记录、不执行
pub fn dry_run() -> bool {
    *DRY_RUN
}

/// 记一个事件；没开记录时什么都不做
pub fn event(kind: &str, values: &[f64]) {
    let Some(out) = OUT.as_ref() else { return };
    let us = START.elapsed().as_secs_f64() * 1e6;
    let mut out = out.lock().unwrap();
    let _ = write!(out, "{us:.0} {kind}");
    for value in values {
        let _ = write!(out, " {value}");
    }
    let _ = writeln!(out);
    let _ = out.flush();
}
