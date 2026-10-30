//! 蒜鸟电脑端检查新版本。
//!
//! 每次发版，GitHub 的 Release 里都带一个 latest.json（`release.yml` 生成），写着两端的新版本号、
//! 安装包的下载地址、大小和 SHA-256，还有发版时用私钥给它签的名 latest.json.sig；
//! 发完再同步一份到 Gitee（国内连 GitHub 慢，`.github/gitee-sync.py`）。
//! 后台定时读它（先读 Gitee 的，读不到再读 GitHub 的），有新版本就在托盘提示；主界面弹窗问要不要更新，
//! 用户点了「立即更新」才下载安装包（先从 Gitee 下，不行再从 GitHub 下）、核对指纹、运行安装包。
//!
//! 只认签名对得上的 latest.json（公钥 `UPDATE_PUBLIC_KEY` 内置在这里，私钥只在发版用的 GitHub 加密变量里）：
//! Gitee 上的那份没签名、签名不对都不用；GitHub 上旧的发布没有签名，从 GitHub 读的没签名时照旧认（https 连的就是 GitHub）。
//! 安装包按 latest.json 里的大小和 SHA-256 核对，所以从哪下载都一样可靠。
//!
//! 读到的结果和用户的选择（跳过这个版本、以后再说、自动检查开关）记在 `%APPDATA%\SuanNIAO\update.json`，
//! 后台和主界面都读写这个文件。

use std::ffi::c_void;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use suanniao_config as config;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Cryptography::{
    BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash, BCryptHashData, BCRYPT_HASH_HANDLE, BCRYPT_SHA256_ALG_HANDLE,
};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

/// 最新一次发布里的 latest.json（GitHub 会跳转到真正的下载地址）
pub const MANIFEST_URL: &str = "https://github.com/drhhhao/SuanNIAO/releases/latest/download/latest.json";

/// Gitee 上最新一版的 latest.json（Gitee 的 latest 指最新建的发行版；国内快，先读这个）
pub const GITEE_MANIFEST_URL: &str = "https://gitee.com/drhhhao_0/SuanNIAO/releases/download/latest/latest.json";

/// latest.json 里的 mirror（Gitee 上这一版的下载地址）只认这个开头的
const MIRROR_PREFIX: &str = "https://gitee.com/drhhhao_0/SuanNIAO/releases/download/";

/// 给 latest.json 签名的 Ed25519 公钥（私钥在 GitHub 加密变量 SUANNIAO_UPDATE_KEY 里，原件和发版密钥放在一起）
const UPDATE_PUBLIC_KEY: [u8; 32] = [
    0xd7, 0xeb, 0x6b, 0xa0, 0x0c, 0xf2, 0x68, 0x16, 0xe5, 0xe0, 0x84, 0x19, 0x29, 0x18, 0x95, 0xe9, 0xf7, 0xc1, 0x0a, 0x22, 0x0b,
    0xdc, 0xfd, 0x7a, 0x3d, 0x85, 0x32, 0x73, 0x50, 0x80, 0xc9, 0xe9,
];

/// 最新一次发布的网页（latest.json 里没写发布页时，「去网页下载」打开这个）
pub const RELEASES_PAGE: &str = "https://github.com/drhhhao/SuanNIAO/releases/latest";

/// 测试用：环境变量 SUANNIAO_UPDATE_URL 换一个地址读 latest.json（比如本机起的 http 服务）
const URL_ENV: &str = "SUANNIAO_UPDATE_URL";

/// 用户取消下载时 download 返回的错误
pub const CANCELLED: &str = "已取消";

/// latest.json：一次发布的两端版本
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Manifest {
    pub schema: u32,
    /// 发布的名字，像 pc0.7.7-android0.9.6
    pub tag: String,
    pub title: String,
    /// 发布时间（ISO 8601，UTC）
    pub published: String,
    /// 发布页地址
    pub page: String,
    /// 这次更新了什么（CHANGELOG.md 里这一版那一段的每一条，两端的都在）
    pub notes: Vec<String>,
    pub pc: Asset,
    pub android: Asset,
    /// Gitee 上这一版的下载地址（以 / 结尾，后面接文件名；没同步过去就是空的）
    pub mirror: String,
}

impl Manifest {
    /// 电脑端这次改了什么：CHANGELOG.md 里分开写了就只要电脑端那几条，没分开写就是整段
    pub fn pc_notes(&self) -> &[String] {
        if self.pc.notes.is_empty() {
            &self.notes
        } else {
            &self.pc.notes
        }
    }
}

/// 一端的安装包
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Asset {
    pub version: String,
    /// 手机端的 versionCode（电脑端没有）
    pub code: u32,
    /// 文件名
    pub file: String,
    pub url: String,
    /// 字节数
    pub size: u64,
    /// SHA-256（小写十六进制）
    pub sha256: String,
    /// 这一端这次改了什么（CHANGELOG.md 里「电脑端」「手机端」下面的几条；没分开写时是空的）
    pub notes: Vec<String>,
}

/// update.json：查到的结果和用户的选择
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// 自动检查更新（主界面「关于」里的开关）
    pub auto: bool,
    /// 上次检查的时间（Unix 秒，没查成也算）
    pub checked: u64,
    /// 上次没查成的原因，查成了就清空
    pub error: String,
    /// 上次查到的 latest.json
    pub latest: Option<Manifest>,
    /// 用户点了「跳过这个版本」的电脑端版本号
    pub skip: String,
    /// 用户点「以后再说」的时间（Unix 秒）：之后一天里不再自己弹出来
    pub later: u64,
    /// 主动提示过的电脑端版本号（托盘通知或者主界面自己弹出的更新窗口），一个版本只主动提示一次
    pub notified: String,
}

impl Default for State {
    fn default() -> Self {
        Self { auto: true, checked: 0, error: String::new(), latest: None, skip: String::new(), later: 0, notified: String::new() }
    }
}

impl State {
    /// 查到的电脑端新版本（比 current 新才算）
    pub fn available(&self, current: &str) -> Option<&Manifest> {
        self.latest.as_ref().filter(|m| newer(&m.pc.version, current))
    }

    /// 有新版本、用户也没跳过它：托盘菜单里显示「更新到 x」，主界面「关于」上有小绿点
    pub fn offered(&self, current: &str) -> Option<&Manifest> {
        self.available(current).filter(|m| m.pc.version != self.skip)
    }
}

pub fn state_path() -> PathBuf {
    config::data_dir().join("update.json")
}

/// 读 update.json（没有或者读不出来就是默认值：自动检查开着，什么都没查到）
pub fn load() -> State {
    std::fs::read_to_string(state_path()).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

/// 改 update.json，返回改完的。后台和主界面都会改：读、改、写整个过程用一个命名互斥量排队，免得互相盖掉
pub fn update(change: impl FnOnce(&mut State)) -> State {
    let _lock = Lock::take();
    let mut state = load();
    change(&mut state);
    let path = state_path();
    if let Ok(text) = serde_json::to_string_pretty(&state) {
        let _ = std::fs::create_dir_all(config::data_dir());
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
    state
}

struct Lock(Option<HANDLE>);

impl Lock {
    fn take() -> Self {
        unsafe {
            let handle = CreateMutexW(None, false, w!("Local\\SuanNIAO.Update")).ok();
            if let Some(handle) = handle {
                WaitForSingleObject(handle, 3000);
            }
            Self(handle)
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if let Some(handle) = self.0 {
            unsafe {
                let _ = ReleaseMutex(handle);
                let _ = CloseHandle(handle);
            }
        }
    }
}

/// 版本号 "0.7.12" → [0, 7, 12, 0]（看不懂的位当 0）
pub fn version_tuple(version: &str) -> [u32; 4] {
    let mut parts = [0; 4];
    for (slot, part) in parts.iter_mut().zip(version.trim().trim_start_matches(['v', 'V']).split('.')) {
        *slot = part.trim().parse().unwrap_or(0);
    }
    parts
}

/// candidate 比 current 新
pub fn newer(candidate: &str, current: &str) -> bool {
    !candidate.trim().is_empty() && version_tuple(candidate) > version_tuple(current)
}

/// 联网读一次 latest.json，结果记进 update.json（没查成也记下时间和原因）
pub fn check() -> Result<Manifest, String> {
    let result = fetch_manifest();
    update(|state| {
        state.checked = config::now_secs();
        match &result {
            Ok(manifest) => {
                state.error.clear();
                state.latest = Some(manifest.clone());
            }
            Err(e) => state.error = e.clone(),
        }
    });
    result
}

fn test_url() -> Option<String> {
    std::env::var(URL_ENV).ok().filter(|url| !url.trim().is_empty())
}

/// 先读 Gitee 的（一定要有对得上的签名），读不到再读 GitHub 的；都没读到时说 GitHub 那边的原因
fn fetch_manifest() -> Result<Manifest, String> {
    if let Some(url) = test_url() {
        return fetch_from(&url, false);
    }
    fetch_from(GITEE_MANIFEST_URL, true).or_else(|_| fetch_from(MANIFEST_URL, false))
}

/// 读一份 latest.json 和它的签名（同一个地址后面加 .sig）。need_signature：没有签名就不认
fn fetch_from(url: &str, need_signature: bool) -> Result<Manifest, String> {
    let body = match get_small(url)? {
        (200, body) => body,
        (404, _) => return Err("发布页上还没有版本信息".into()),
        (status, _) => return Err(format!("服务器返回 {status}")),
    };
    match get_small(&format!("{url}.sig"))? {
        (200, signature) => {
            if !signed(&UPDATE_PUBLIC_KEY, &body, &signature) {
                return Err("版本信息的签名对不上，没用它".into());
            }
        }
        (404, _) if !need_signature => {}
        (status, _) => return Err(format!("读不到版本信息的签名（服务器返回 {status}）")),
    }
    let mut manifest: Manifest = serde_json::from_slice(&body).map_err(|_| "版本信息看不懂".to_string())?;
    // 只认 https 的下载地址；测试时换了地址才认本机的 http
    let test = test_url().is_some();
    let url_ok = |url: &str| url.starts_with("https://") || (test && url.starts_with("http://"));
    if manifest.pc.version.trim().is_empty() || !url_ok(&manifest.pc.url) || manifest.pc.sha256.trim().len() != 64 {
        return Err("版本信息不完整".into());
    }
    let mirror_ok = manifest.mirror.starts_with(MIRROR_PREFIX) || (test && url_ok(&manifest.mirror));
    if !mirror_ok || !manifest.mirror.ends_with('/') {
        manifest.mirror.clear();
    }
    Ok(manifest)
}

/// GET 一个小文件（版本信息、签名，最多 1 MB），返回状态码和内容
fn get_small(url: &str) -> Result<(u32, Vec<u8>), String> {
    let mut response = Response::get(url, true)?;
    let status = response.status;
    let body = if status == 200 { response.read_to_end(1 << 20)? } else { Vec::new() };
    Ok((status, body))
}

/// signature 是 key 对 message 的 Ed25519 签名
fn signed(key: &[u8; 32], message: &[u8], signature: &[u8]) -> bool {
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key).verify(message, signature).is_ok()
}

/// 下载的安装包放在哪个文件夹：临时文件夹里的 SuanNIAO-update（装完就没用了，主界面过一会儿删掉）
pub fn download_dir() -> PathBuf {
    std::env::temp_dir().join("SuanNIAO-update")
}

/// 安装包下载到哪：download_dir 里的同名文件
pub fn installer_path(asset: &Asset) -> PathBuf {
    let name = Path::new(&asset.file).file_name().map(|name| name.to_owned()).unwrap_or_else(|| "SuanNIAO-Setup.exe".into());
    download_dir().join(name)
}

/// 下载安装包到 dest：先写到 .part，大小和 SHA-256 都对上了才改成 dest；dest 已经是下好的同一个文件就不再下。
/// mirror 不是空的就先从那里（Gitee）下，没下成、对不上再从 asset.url（GitHub）下。
/// progress(已下载, 总共) 在调用的线程上调；cancel 变成 true 就停下，返回 CANCELLED
pub fn download(asset: &Asset, mirror: &str, dest: &Path, cancel: &AtomicBool, mut progress: impl FnMut(u64, u64)) -> Result<(), String> {
    if sha256_file(dest).is_some_and(|hash| hash.eq_ignore_ascii_case(asset.sha256.trim())) {
        progress(asset.size, asset.size);
        return Ok(());
    }
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建不了下载文件夹：{e}"))?;
    }
    let part = dest.with_extension("part");
    let mut sources = Vec::new();
    if !mirror.is_empty() && !asset.file.is_empty() {
        sources.push(format!("{mirror}{}", asset.file));
    }
    sources.push(asset.url.clone());
    let mut result = Err(String::new());
    for url in &sources {
        result = fetch_to(url, asset, &part, cancel, &mut progress)
            .and_then(|()| std::fs::rename(&part, dest).map_err(|e| format!("存不了下载的文件：{e}")));
        if result.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        if result.is_ok() || result.as_ref().is_err_and(|e| e == CANCELLED) {
            break;
        }
    }
    result
}

fn fetch_to(url: &str, asset: &Asset, part: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    let mut response = Response::get(url, false)?;
    if response.status != 200 {
        return Err(format!("服务器返回 {}", response.status));
    }
    let total = if asset.size > 0 { asset.size } else { response.length.unwrap_or(0) };
    let mut file = std::fs::File::create(part).map_err(|e| format!("存不了下载的文件：{e}"))?;
    let mut hash = Sha256::new()?;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    progress(0, total);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        let n = response.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n]).map_err(|e| format!("存不了下载的文件：{e}"))?;
        hash.update(&buffer[..n]);
        done += n as u64;
        progress(done, total);
    }
    drop(file);
    if asset.size > 0 && done != asset.size {
        return Err(format!("文件没下完整（{done} / {} 字节）", asset.size));
    }
    if !hash.finish().eq_ignore_ascii_case(asset.sha256.trim()) {
        return Err("下载的文件和发布的对不上，没有安装".into());
    }
    Ok(())
}

/// 文件的 SHA-256（小写十六进制），读不了返回 None
pub fn sha256_file(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hash = Sha256::new().ok()?;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buffer).ok()?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Some(hash.finish())
}

/// SHA-256（系统自带的 BCrypt）
struct Sha256(BCRYPT_HASH_HANDLE);

impl Sha256 {
    fn new() -> Result<Self, String> {
        let mut handle = BCRYPT_HASH_HANDLE::default();
        let status = unsafe { BCryptCreateHash(BCRYPT_SHA256_ALG_HANDLE, &mut handle, None, None, 0) };
        if status.0 < 0 {
            return Err(format!("算不了文件指纹（{:#x}）", status.0));
        }
        Ok(Self(handle))
    }

    fn update(&mut self, data: &[u8]) {
        unsafe {
            let _ = BCryptHashData(self.0, data, 0);
        }
    }

    fn finish(self) -> String {
        let mut digest = [0u8; 32];
        unsafe {
            let _ = BCryptFinishHash(self.0, &mut digest, 0);
        }
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl Drop for Sha256 {
    fn drop(&mut self) {
        unsafe {
            let _ = BCryptDestroyHash(self.0);
        }
    }
}

/// 拆开的网址：http(s)://主机[:端口]/路径
struct Target {
    secure: bool,
    host: String,
    port: u16,
    path: String,
}

impl Target {
    fn parse(url: &str) -> Result<Self, String> {
        let bad = || format!("网址不对：{url}");
        let (secure, rest) = match url.trim().split_once("://") {
            Some(("https", rest)) => (true, rest),
            Some(("http", rest)) => (false, rest),
            _ => return Err(bad()),
        };
        let (authority, path) = rest.find('/').map_or((rest, "/"), |i| rest.split_at(i));
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().map_err(|_| bad())?),
            None => (authority, if secure { 443 } else { 80 }),
        };
        if host.is_empty() {
            return Err(bad());
        }
        Ok(Self { secure, host: host.into(), port, path: path.into() })
    }
}

/// WinHTTP 的句柄，用完关掉
struct Internet(*mut c_void);

impl Internet {
    fn new(handle: *mut c_void) -> Result<Self, String> {
        if handle.is_null() {
            Err(describe(unsafe { GetLastError() }.0))
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for Internet {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// 一次 GET 请求（字段按关的顺序排：先关请求，再关连接和会话）
struct Response {
    request: Internet,
    _connect: Internet,
    _session: Internet,
    status: u32,
    length: Option<u64>,
}

impl Response {
    /// 发 GET 请求，等到回应头。会跟着跳转走（GitHub 的下载地址要跳两次），代理用系统的设置。
    /// fresh：不要缓存（读版本信息时用，免得代理给一份旧的）
    fn get(url: &str, fresh: bool) -> Result<Self, String> {
        let target = Target::parse(url)?;
        unsafe {
            let session = Internet::new(WinHttpOpen(w!("SuanNIAO"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0))?;
            let _ = WinHttpSetTimeouts(session.0, 10_000, 15_000, 30_000, 30_000);
            let connect = Internet::new(WinHttpConnect(session.0, &HSTRING::from(target.host.as_str()), target.port, 0))?;
            let mut flags = if target.secure { WINHTTP_FLAG_SECURE.0 } else { 0 };
            if fresh {
                flags |= WINHTTP_FLAG_REFRESH.0;
            }
            let request = Internet::new(WinHttpOpenRequest(
                connect.0,
                w!("GET"),
                &HSTRING::from(target.path.as_str()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_OPEN_REQUEST_FLAGS(flags),
            ))?;
            WinHttpSendRequest(request.0, None, None, 0, 0, 0).map_err(net_error)?;
            WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(net_error)?;
            let mut status = 0u32;
            let mut size = 4u32;
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some(&mut status as *mut u32 as *mut c_void),
                &mut size,
                std::ptr::null_mut(),
            )
            .map_err(net_error)?;
            let mut length = 0u64;
            let mut size = 8u32;
            let length = WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER64,
                PCWSTR::null(),
                Some(&mut length as *mut u64 as *mut c_void),
                &mut size,
                std::ptr::null_mut(),
            )
            .ok()
            .map(|()| length);
            Ok(Self { request, _connect: connect, _session: session, status, length })
        }
    }

    /// 读一块，读完了返回 0
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, String> {
        let mut read = 0u32;
        unsafe { WinHttpReadData(self.request.0, buffer.as_mut_ptr() as *mut c_void, buffer.len() as u32, &mut read) }.map_err(net_error)?;
        Ok(read as usize)
    }

    fn read_to_end(&mut self, limit: usize) -> Result<Vec<u8>, String> {
        let mut body = Vec::new();
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let n = self.read(&mut buffer)?;
            if n == 0 {
                return Ok(body);
            }
            body.extend_from_slice(&buffer[..n]);
            if body.len() > limit {
                return Err("版本信息太大，不像是对的".into());
            }
        }
    }
}

fn net_error(e: windows::core::Error) -> String {
    describe((e.code().0 as u32) & 0xFFFF)
}

/// WinHTTP 的错误码说成人话
fn describe(code: u32) -> String {
    match code {
        12002 => "连接超时，网络不好或者连不上下载的服务器".into(),
        12007 => "找不到服务器，网络没连上？".into(),
        12029 => "连不上服务器，网络不好或者被拦住了".into(),
        12030 | 12152 => "和服务器的连接断了".into(),
        12175 => "安全连接没建起来（证书不对，可能被代理拦了）".into(),
        _ => format!("网络出错（{code}）"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("0.7.7", "0.7.6"));
        assert!(newer("0.7.10", "0.7.9"));
        assert!(newer("0.8.0", "0.7.12"));
        assert!(!newer("0.7.6", "0.7.6"));
        assert!(!newer("0.7.6.0", "0.7.6"));
        assert!(!newer("0.7.5", "0.7.6"));
        assert!(!newer("", "0.7.6"));
        assert_eq!(version_tuple("v1.2"), [1, 2, 0, 0]);
    }

    #[test]
    fn urls() {
        let t = Target::parse("https://github.com/drhhhao/SuanNIAO/releases/latest/download/latest.json").unwrap();
        assert!(t.secure && t.host == "github.com" && t.port == 443 && t.path == "/drhhhao/SuanNIAO/releases/latest/download/latest.json");
        let t = Target::parse("http://127.0.0.1:8765/latest.json").unwrap();
        assert!(!t.secure && t.host == "127.0.0.1" && t.port == 8765 && t.path == "/latest.json");
        let t = Target::parse("https://example.com").unwrap();
        assert!(t.port == 443 && t.path == "/");
        assert!(Target::parse("ftp://example.com/x").is_err());
        assert!(Target::parse("https://:80/x").is_err());
    }

    #[test]
    fn sha256() {
        let mut hash = Sha256::new().unwrap();
        hash.update(b"abc");
        assert_eq!(hash.finish(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn ed25519() {
        // RFC 8032 第 7.1 节的第一组：空消息
        let key: [u8; 32] = hex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a").try_into().unwrap();
        let sig = hex("e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b");
        assert!(signed(&key, b"", &sig));
        assert!(!signed(&key, b"x", &sig));
        assert!(!signed(&UPDATE_PUBLIC_KEY, b"", &sig));
        assert!(!signed(&key, b"", &sig[..63]));
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn offered_respects_skip() {
        let mut state = State::default();
        state.latest = Some(Manifest { pc: Asset { version: "0.7.8".into(), ..Default::default() }, ..Default::default() });
        assert!(state.offered("0.7.7").is_some());
        assert!(state.available("0.7.8").is_none());
        state.skip = "0.7.8".into();
        assert!(state.offered("0.7.7").is_none());
        assert!(state.available("0.7.7").is_some());
    }
}
