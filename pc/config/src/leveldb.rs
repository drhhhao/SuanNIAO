//! 读 Chromium（WebView2）localStorage 用的 LevelDB，只为从本机 axonkey 导入映射。
//!
//! axonkey 是 WebView 应用，映射存在 `%LOCALAPPDATA%\com.axonkey.app\EBWebView\Default\Local Storage\leveldb`，
//! 键是 `_http://tauri.localhost\0\x01axonkey.mapping-profiles.v1`，值的第一个字节 0 = UTF-16LE、1 = Latin-1。
//! .ldb 的数据块用 Snappy 压缩，最新的写入可能还在 .log 里，两种都读，同一个键取序列号最大的。
//! axonkey 运行时锁着数据库，所以先把文件拷到临时目录再读；只读，绝不写 axonkey 的数据。

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// 本机 axonkey 的 localStorage 目录
pub fn axonkey_dir() -> PathBuf {
    PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join(r"com.axonkey.app\EBWebView\Default\Local Storage\leveldb")
}

/// axonkey 现在用的 origin（旧的 http://localhost:1420 是改版前的旧数据，不读）
const ORIGIN: &str = "_http://tauri.localhost";

/// 读本机 axonkey 的映射方案（axonkey.mapping-profiles.v1 的 JSON 文本）。先拷到临时目录再读
pub fn read_axonkey_profiles() -> Result<String, String> {
    let source = axonkey_dir();
    if !source.is_dir() {
        return Err("这台电脑上没找到遥控器映射软件的数据（装过并打开过才有）".into());
    }
    let temp = std::env::temp_dir().join(format!("suanniao-import-{}-{}", std::process::id(), crate::now_secs()));
    let result = (|| {
        fs::create_dir_all(&temp).map_err(|e| format!("建临时目录失败：{e}"))?;
        for entry in fs::read_dir(&source).map_err(|e| format!("读数据文件夹失败：{e}"))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
            if name.ends_with(".ldb") || name.ends_with(".log") {
                fs::copy(&path, temp.join(path.file_name().unwrap())).map_err(|e| format!("拷贝 {name} 失败：{e}"))?;
            }
        }
        let entries = read_dir(&temp).map_err(|e| format!("解析数据失败：{e}"))?;
        for name in ["axonkey.mapping-profiles.v1", "axonkey.settings.v1"] {
            let mut key = ORIGIN.as_bytes().to_vec();
            key.extend_from_slice(b"\x00\x01");
            key.extend_from_slice(name.as_bytes());
            if let Some(Some(value)) = entries.get(&key) {
                return Ok(decode_value(value));
            }
        }
        Err("里面还没有映射（还没在遥控器映射软件里改过按键？）".into())
    })();
    let _ = fs::remove_dir_all(&temp);
    result
}

/// localStorage 的值：第一个字节 0 = UTF-16LE，1 = Latin-1
fn decode_value(value: &[u8]) -> String {
    match value.split_first() {
        Some((0, rest)) => String::from_utf16_lossy(&rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()),
        Some((1, rest)) => rest.iter().map(|&b| b as char).collect(),
        _ => value.iter().map(|&b| b as char).collect(),
    }
}

/// 读一个目录里的 .ldb 和 .log：每个键最新的值（None = 已删除）
pub fn read_dir(dir: &Path) -> io::Result<HashMap<Vec<u8>, Option<Vec<u8>>>> {
    let mut latest: HashMap<Vec<u8>, (u64, Option<Vec<u8>>)> = HashMap::new();
    let mut put = |key: Vec<u8>, seq: u64, value: Option<Vec<u8>>| {
        if latest.get(&key).is_none_or(|(s, _)| *s < seq) {
            latest.insert(key, (seq, value));
        }
    };
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
        let data = fs::read(&path)?;
        // 某个文件坏了就跳过它，别的照读
        if name.ends_with(".ldb") {
            let _ = read_table(&data, &mut put);
        } else if name.ends_with(".log") {
            let _ = read_log(&data, &mut put);
        }
    }
    Ok(latest.into_iter().map(|(k, (_, v))| (k, v)).collect())
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

fn varint(buf: &[u8], pos: &mut usize) -> io::Result<u64> {
    let mut result = 0u64;
    let mut shift = 0;
    loop {
        let b = *buf.get(*pos).ok_or_else(|| bad("varint"))?;
        *pos += 1;
        result |= u64::from(b & 0x7F) << shift;
        if b < 0x80 {
            return Ok(result);
        }
        shift += 7;
        if shift > 63 {
            return Err(bad("varint"));
        }
    }
}

/// Snappy 解压（LevelDB 数据块用的格式）
pub fn snappy(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut pos = 0;
    let len = varint(data, &mut pos)? as usize;
    let mut out: Vec<u8> = Vec::with_capacity(len);
    while pos < data.len() {
        let tag = data[pos];
        pos += 1;
        let (length, offset) = match tag & 3 {
            0 => {
                let mut n = (tag >> 2) as usize;
                if n >= 60 {
                    let bytes = n - 59;
                    let raw = data.get(pos..pos + bytes).ok_or_else(|| bad("snappy"))?;
                    n = raw.iter().rev().fold(0usize, |acc, &b| (acc << 8) | b as usize);
                    pos += bytes;
                }
                let n = n + 1;
                out.extend_from_slice(data.get(pos..pos + n).ok_or_else(|| bad("snappy"))?);
                pos += n;
                continue;
            }
            1 => {
                let b = *data.get(pos).ok_or_else(|| bad("snappy"))? as usize;
                pos += 1;
                (((tag >> 2) & 7) as usize + 4, (((tag >> 5) as usize) << 8) | b)
            }
            2 => {
                let raw = data.get(pos..pos + 2).ok_or_else(|| bad("snappy"))?;
                pos += 2;
                ((tag >> 2) as usize + 1, u16::from_le_bytes([raw[0], raw[1]]) as usize)
            }
            _ => {
                let raw = data.get(pos..pos + 4).ok_or_else(|| bad("snappy"))?;
                pos += 4;
                ((tag >> 2) as usize + 1, u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize)
            }
        };
        if offset == 0 || offset > out.len() {
            return Err(bad("snappy offset"));
        }
        let start = out.len() - offset;
        for i in 0..length {
            let b = out[start + i];
            out.push(b);
        }
    }
    if out.len() != len {
        return Err(bad("snappy length"));
    }
    Ok(out)
}

fn read_block(data: &[u8], offset: usize, size: usize) -> io::Result<Vec<u8>> {
    let block = data.get(offset..offset + size).ok_or_else(|| bad("block"))?;
    match data.get(offset + size) {
        Some(0) => Ok(block.to_vec()),
        Some(1) => snappy(block),
        _ => Err(bad("block compression")),
    }
}

/// 数据块里的（键，值），按前缀压缩还原
fn block_entries(block: &[u8]) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    if block.len() < 4 {
        return Err(bad("block"));
    }
    let restarts = u32::from_le_bytes(block[block.len() - 4..].try_into().unwrap()) as usize;
    let end = block.len().checked_sub(4 + 4 * restarts).ok_or_else(|| bad("block restarts"))?;
    let mut pos = 0;
    let mut key: Vec<u8> = Vec::new();
    let mut out = Vec::new();
    while pos < end {
        let shared = varint(block, &mut pos)? as usize;
        let unshared = varint(block, &mut pos)? as usize;
        let vlen = varint(block, &mut pos)? as usize;
        key.truncate(shared);
        key.extend_from_slice(block.get(pos..pos + unshared).ok_or_else(|| bad("block key"))?);
        pos += unshared;
        out.push((key.clone(), block.get(pos..pos + vlen).ok_or_else(|| bad("block value"))?.to_vec()));
        pos += vlen;
    }
    Ok(out)
}

/// .ldb（SSTable）：表尾 48 字节里有索引块的位置，索引块里是各数据块的位置
fn read_table(data: &[u8], put: &mut impl FnMut(Vec<u8>, u64, Option<Vec<u8>>)) -> io::Result<()> {
    let footer = data.get(data.len().checked_sub(48).ok_or_else(|| bad("table"))?..).ok_or_else(|| bad("table"))?;
    let mut pos = 0;
    let _meta_offset = varint(footer, &mut pos)?;
    let _meta_size = varint(footer, &mut pos)?;
    let index_offset = varint(footer, &mut pos)? as usize;
    let index_size = varint(footer, &mut pos)? as usize;
    for (_, handle) in block_entries(&read_block(data, index_offset, index_size)?)? {
        let mut p = 0;
        let offset = varint(&handle, &mut p)? as usize;
        let size = varint(&handle, &mut p)? as usize;
        for (internal, value) in block_entries(&read_block(data, offset, size)?)? {
            if internal.len() < 8 {
                continue;
            }
            let (user, trailer) = internal.split_at(internal.len() - 8);
            let trailer = u64::from_le_bytes(trailer.try_into().unwrap());
            let value = (trailer & 0xFF == 1).then_some(value);
            put(user.to_vec(), trailer >> 8, value);
        }
    }
    Ok(())
}

/// .log：32 KB 一块，每条记录有 7 字节头（校验、长度、类型），拼成写批次再逐条读
fn read_log(data: &[u8], put: &mut impl FnMut(Vec<u8>, u64, Option<Vec<u8>>)) -> io::Result<()> {
    const BLOCK: usize = 32768;
    let mut pos = 0;
    let mut fragment: Vec<u8> = Vec::new();
    let mut batches: Vec<Vec<u8>> = Vec::new();
    while pos + 7 <= data.len() {
        let left = BLOCK - pos % BLOCK;
        if left < 7 {
            pos += left;
            continue;
        }
        let length = u16::from_le_bytes([data[pos + 4], data[pos + 5]]) as usize;
        let kind = data[pos + 6];
        let payload = data.get(pos + 7..pos + 7 + length).unwrap_or(&data[(pos + 7).min(data.len())..]);
        pos += 7 + length;
        match kind {
            1 => batches.push(payload.to_vec()),
            2 => fragment = payload.to_vec(),
            3 => fragment.extend_from_slice(payload),
            4 => {
                fragment.extend_from_slice(payload);
                batches.push(std::mem::take(&mut fragment));
            }
            _ => {}
        }
    }
    for batch in batches {
        if batch.len() < 12 {
            continue;
        }
        let seq = u64::from_le_bytes(batch[..8].try_into().unwrap());
        let count = u32::from_le_bytes(batch[8..12].try_into().unwrap()) as u64;
        let mut p = 12;
        for i in 0..count {
            let Some(&tag) = batch.get(p) else { break };
            p += 1;
            let klen = varint(&batch, &mut p)? as usize;
            let key = batch.get(p..p + klen).ok_or_else(|| bad("log key"))?.to_vec();
            p += klen;
            let value = if tag == 1 {
                let vlen = varint(&batch, &mut p)? as usize;
                let v = batch.get(p..p + vlen).ok_or_else(|| bad("log value"))?.to_vec();
                p += vlen;
                Some(v)
            } else {
                None
            };
            put(key, seq + i, value);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snappy_roundtrip_literal_and_copy() {
        // "abcabcabcabc"：先 3 个字面字节，再一个偏移 3、长度 9 的复制（会和自己重叠）
        let data = [12, 0b0000_1000, b'a', b'b', b'c', (5 << 2) | 1, 3];
        assert_eq!(snappy(&data).unwrap(), b"abcabcabcabc");
    }

    /// 用之前从用户机器上拷出来的 axonkey 数据测（%TEMP%\axonkey-scratch\ldb），没有就跳过
    #[test]
    fn reads_copied_axonkey_data() {
        let dir = std::env::temp_dir().join(r"axonkey-scratch\ldb");
        if !dir.is_dir() {
            return;
        }
        let entries = read_dir(&dir).unwrap();
        let mut key = ORIGIN.as_bytes().to_vec();
        key.extend_from_slice(b"\x00\x01axonkey.mapping-profiles.v1");
        let value = entries.get(&key).cloned().flatten().expect("profiles key");
        let text = decode_value(&value);
        let import = crate::axonkey::parse(&text).unwrap();
        assert_eq!(import.profile, "默认配置");
        assert!(import.keys.iter().any(|k| k.id == "home" && k.configured));
    }
}
