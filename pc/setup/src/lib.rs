//! 蒜鸟电脑端安装包的格式。
//!
//! 安装包 = 启动程序 SuanNIAO-Setup.exe + 压缩好的文件 + 72 字节的尾巴。
//! 尾巴：16 字节的标记，压缩数据从文件第几个字节开始、压缩后多大、解开后多大（各一个 u64，小端），解开后的 SHA-256（32 字节）。
//! 解开后是几个文件首尾相接，每个是 [名字的字节数 u16][名字 UTF-8][内容的字节数 u64][内容]。
//! 压缩用系统自带的 LZMS（cabinet.dll 里的 Compression API），打包工具和启动程序都不用别的库。

use std::ffi::c_void;

use windows::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};
use windows::Win32::Storage::Compression::{
    CloseCompressor, CloseDecompressor, Compress, CreateCompressor, CreateDecompressor, Decompress, COMPRESSOR_HANDLE,
    COMPRESS_ALGORITHM_LZMS, DECOMPRESSOR_HANDLE,
};

const MAGIC: &[u8; 16] = b"SuanNIAO-Payload";
const TRAILER_LEN: usize = 16 + 8 * 3 + 32;
/// 解开后最大多少（防止尾巴坏了要一大块内存）
const RAW_MAX: usize = 1 << 30;

/// 安装包里的一个文件
pub struct Entry {
    /// 只是文件名，不带路径
    pub name: String,
    pub data: Vec<u8>,
}

/// 打安装包：启动程序 + 压缩好的文件 + 尾巴
pub fn build(stub: &[u8], entries: &[Entry]) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    for entry in entries {
        if !valid_name(&entry.name) {
            return Err(format!("文件名不对：{}", entry.name));
        }
        raw.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        raw.extend_from_slice(entry.name.as_bytes());
        raw.extend_from_slice(&(entry.data.len() as u64).to_le_bytes());
        raw.extend_from_slice(&entry.data);
    }
    let packed = compress(&raw)?;
    let mut out = Vec::with_capacity(stub.len() + packed.len() + TRAILER_LEN);
    out.extend_from_slice(stub);
    let offset = out.len() as u64;
    out.extend_from_slice(&packed);
    out.extend_from_slice(MAGIC);
    for number in [offset, packed.len() as u64, raw.len() as u64] {
        out.extend_from_slice(&number.to_le_bytes());
    }
    out.extend_from_slice(&sha256(&raw)?);
    Ok(out)
}

/// 从安装包里取出文件（解开后核对过指纹）
pub fn open(installer: &[u8]) -> Result<Vec<Entry>, String> {
    let broken = || "安装包不完整，可能没下载完，请重新下载".to_string();
    let body_len = installer.len().checked_sub(TRAILER_LEN).ok_or_else(broken)?;
    let trailer = &installer[body_len..];
    if &trailer[..16] != MAGIC {
        return Err(broken());
    }
    let number = |i: usize| u64::from_le_bytes(trailer[16 + i * 8..24 + i * 8].try_into().unwrap()) as usize;
    let (offset, packed_len, raw_len) = (number(0), number(1), number(2));
    if offset.checked_add(packed_len) != Some(body_len) || raw_len > RAW_MAX {
        return Err(broken());
    }
    let raw = decompress(&installer[offset..body_len], raw_len).map_err(|_| broken())?;
    if sha256(&raw)?[..] != trailer[40..] {
        return Err(broken());
    }
    parse(&raw).ok_or_else(broken)
}

fn parse(mut raw: &[u8]) -> Option<Vec<Entry>> {
    let mut entries = Vec::new();
    while !raw.is_empty() {
        let name_len = u16::from_le_bytes(raw.get(..2)?.try_into().ok()?) as usize;
        let name = std::str::from_utf8(raw.get(2..2 + name_len)?).ok()?.to_string();
        raw = &raw[2 + name_len..];
        let len = usize::try_from(u64::from_le_bytes(raw.get(..8)?.try_into().ok()?)).ok()?;
        let data = raw.get(8..8usize.checked_add(len)?)?.to_vec();
        raw = &raw[8 + len..];
        if !valid_name(&name) {
            return None;
        }
        entries.push(Entry { name, data });
    }
    Some(entries)
}

/// 只能是文件名：解压时直接拼在临时文件夹后面，不能带路径跑到别处
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= u16::MAX as usize && !name.contains(['/', '\\', ':']) && name != "." && name != ".."
}

fn compress(raw: &[u8]) -> Result<Vec<u8>, String> {
    unsafe {
        let mut handle = COMPRESSOR_HANDLE::default();
        CreateCompressor(COMPRESS_ALGORITHM_LZMS, None, &mut handle).map_err(|e| format!("建不了压缩器：{e}"))?;
        // 先问要多大的地方，再压
        let mut needed = 0usize;
        let _ = Compress(handle, Some(raw.as_ptr() as *const c_void), raw.len(), None, 0, &mut needed);
        let mut out = vec![0u8; needed.max(raw.len() / 2 + 4096)];
        let mut written = 0usize;
        let result = Compress(handle, Some(raw.as_ptr() as *const c_void), raw.len(), Some(out.as_mut_ptr() as *mut c_void), out.len(), &mut written);
        let _ = CloseCompressor(handle);
        result.map_err(|e| format!("压缩失败：{e}"))?;
        out.truncate(written);
        Ok(out)
    }
}

fn decompress(packed: &[u8], raw_len: usize) -> Result<Vec<u8>, String> {
    unsafe {
        let mut handle = DECOMPRESSOR_HANDLE::default();
        CreateDecompressor(COMPRESS_ALGORITHM_LZMS, None, &mut handle).map_err(|e| format!("建不了解压器：{e}"))?;
        let mut out = vec![0u8; raw_len];
        let mut written = 0usize;
        let result = Decompress(
            handle,
            Some(packed.as_ptr() as *const c_void),
            packed.len(),
            Some(out.as_mut_ptr() as *mut c_void),
            out.len(),
            Some(&mut written),
        );
        let _ = CloseDecompressor(handle);
        result.map_err(|e| format!("解压失败：{e}"))?;
        if written != raw_len {
            return Err("解开的大小不对".into());
        }
        Ok(out)
    }
}

fn sha256(data: &[u8]) -> Result<[u8; 32], String> {
    let mut digest = [0u8; 32];
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut digest) };
    if status.0 < 0 {
        return Err(format!("算不了指纹（{:#x}）", status.0));
    }
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let entries = vec![
            Entry { name: "SuanNIAO.exe".into(), data: (0..300_000u32).flat_map(|i| (i % 251).to_le_bytes()).collect() },
            Entry { name: "SuanNIAO-core.exe".into(), data: b"core".to_vec() },
            Entry { name: "empty".into(), data: Vec::new() },
        ];
        let installer = build(b"MZstub", &entries).unwrap();
        assert!(installer.starts_with(b"MZstub"));
        let back = open(&installer).unwrap();
        assert_eq!(back.len(), 3);
        for (a, b) in back.iter().zip(&entries) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.data, b.data);
        }
        // 少一个字节、改一个字节都认得出来
        assert!(open(&installer[1..]).is_err());
        let mut bad = installer.clone();
        bad[10] ^= 1;
        assert!(open(&bad).is_err());
        assert!(open(b"short").is_err());
        assert!(build(b"", &[Entry { name: "../x".into(), data: vec![] }]).is_err());
    }
}
