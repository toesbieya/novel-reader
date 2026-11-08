//! EPUB 纯文本缓存（**只服务全文搜索**）。
//!
//! 这是「零副本」不变量的唯一例外：默认在 `%LOCALAPPDATA%\novel-reader\cache\`
//! 存一份纯文本，用来省掉每次搜索时的逐章解压；Tauri 侧会把根目录改到用户数据目录下。
//! 边界很硬：
//! - 只有纯文本，没有 HTML / 样式 / 任何显示用数据；
//! - `read_chapter` 永远走原文件，缓存**不参与显示路径**；
//! - 按 EPUB 的 `size + 内容指纹 + 章节数` 失效，对不上就整份重写；
//! - 整个目录可以随时删掉，最多让下一次搜索慢一点。
//!
//! 落盘格式（小端）：魔数 + size + 内容指纹 + 章节数 + 每章长度和 UTF-8 字节。
//! 指纹由 `epub.rs` 用文件头尾各 4KB + size + mtime 现算，能识别「同尺寸、
//! 同一毫秒内重新打包」这类 mtime 抓不到的改动；旧版缓存里存的是纯 mtime，
//! 字段值对不上会自动失效重建，不需要升格式。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const MAGIC: &[u8; 6] = b"NRSC1\n";
const APP_DIR: &str = "novel-reader";
const CACHE_DIR: &str = "cache";

/// Tauri 侧可以把缓存根目录统一到用户数据目录；未设置时退回 LOCALAPPDATA。
static ROOT: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn set_root(dir: PathBuf) {
    let _ = ROOT.set(dir);
}

fn cache_base() -> Option<PathBuf> {
    if let Some(dir) = ROOT.get() {
        return Some(dir.clone());
    }
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join(APP_DIR).join(CACHE_DIR))
}

/// 某本 EPUB 的缓存文件位置；拿不到缓存根目录时返回 None（退化为不缓存）。
pub(crate) fn cache_path(source: &Path) -> Option<PathBuf> {
    let base = cache_base()?;
    let mut hasher = DefaultHasher::new();
    source.as_os_str().hash(&mut hasher);
    let name = format!("{:016x}.epubtxt", hasher.finish());
    Some(base.join(name))
}

/// 读缓存。`size` / 内容指纹 / 章节数任一不符、文件截断或损坏都当作没有缓存。
pub(crate) fn load(
    path: &Path,
    size: u64,
    stamp: u64,
    chapter_count: usize,
) -> Option<Vec<String>> {
    let data = std::fs::read(path).ok()?;
    decode(&data, size, stamp, chapter_count)
}

/// 写缓存。任何 IO 失败都静默——搜索本身照常工作，只是下次还要重新解压。
pub(crate) fn store(path: &Path, size: u64, stamp: u64, chapters: &[String]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, encode(size, stamp, chapters));
}

fn encode(size: u64, stamp: u64, chapters: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&stamp.to_le_bytes());
    out.extend_from_slice(&(chapters.len() as u64).to_le_bytes());
    for text in chapters {
        out.extend_from_slice(&(text.len() as u64).to_le_bytes());
        out.extend_from_slice(text.as_bytes());
    }
    out
}

fn decode(data: &[u8], size: u64, stamp: u64, chapter_count: usize) -> Option<Vec<String>> {
    let mut r = Reader { data, pos: 0 };
    if r.take(MAGIC.len())? != MAGIC {
        return None;
    }
    if r.u64()? != size || r.u64()? != stamp || r.u64()? != chapter_count as u64 {
        return None;
    }

    let mut out = Vec::with_capacity(chapter_count);
    for _ in 0..chapter_count {
        let len = r.u64()? as usize;
        let bytes = r.take(len)?;
        out.push(std::str::from_utf8(bytes).ok()?.to_string());
    }
    // 尾巴还有数据说明文件被写坏了，宁可重解压
    if r.pos != data.len() {
        return None;
    }
    Some(out)
}

/// 按顺序读定长字段的小工具
struct Reader<'a> {
    /// 完整的缓存文件内容
    data: &'a [u8],
    /// 下一个待读字节的下标
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let chapters = vec!["第一章\n正文。".to_string(), String::new(), "第二章".to_string()];
        let bytes = encode(10, 20, &chapters);
        assert_eq!(decode(&bytes, 10, 20, 3), Some(chapters));
    }

    #[test]
    fn stale_or_broken_cache_is_rejected() {
        let chapters = vec!["第一章".to_string()];
        let bytes = encode(10, 20, &chapters);
        assert_eq!(decode(&bytes, 11, 20, 1), None, "size 变了");
        assert_eq!(decode(&bytes, 10, 21, 1), None, "内容指纹变了");
        assert_eq!(decode(&bytes, 10, 20, 2), None, "章节数变了");
        assert_eq!(decode(&bytes[..bytes.len() - 1], 10, 20, 1), None, "截断就作废");
        let mut extra = bytes.clone();
        extra.push(0);
        assert_eq!(decode(&extra, 10, 20, 1), None, "多出来的尾巴也作废");
    }
}
