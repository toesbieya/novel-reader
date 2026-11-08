//! 集成测试共用的小工具：临时文件与几种常见编码的字节流。
//! 语料全部现场生成，不入库二进制夹具。

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use encoding_rs::GB18030;

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// 临时 .txt 文件，析构时删除（测试 panic 也能清掉）。
pub struct TempTxt {
    /// 实际落盘路径，交给 `TxtBook::open` / `Text::open`
    path: PathBuf,
}

impl TempTxt {
    /// `bytes` 原样写盘，编码由调用方决定；`name` 只用于区分是哪个用例
    pub fn new(name: &str, bytes: &[u8]) -> Self {
        let dir = std::env::temp_dir().join(format!("novel-core-tests-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录失败");
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("{seq}-{name}.txt"));
        std::fs::write(&path, bytes).expect("写临时文件失败");
        Self { path }
    }

    /// 文件路径
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempTxt {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 临时目录，析构时整个删除（SQLite 的 -wal / -shm 一起清掉）。
pub struct TempDir {
    /// 目录路径，用例自己往里放 db / txt
    path: PathBuf,
}

impl TempDir {
    /// 按 `name` 建一个空目录
    pub fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!("novel-core-tests-{}", std::process::id()));
        std::fs::create_dir_all(&base).expect("建临时目录失败");
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!("{seq}-{name}"));
        std::fs::create_dir_all(&path).expect("建临时目录失败");
        Self { path }
    }

    /// 目录路径
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// 按 GB18030 编码（中文网文最常见的非 UTF-8 来源）
pub fn gb18030(s: &str) -> Vec<u8> {
    GB18030.encode(s).0.into_owned()
}

/// 按 UTF-16LE 编码；`bom` 决定是否加 FF FE 头
pub fn utf16le(s: &str, bom: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 2 + 2);
    if bom {
        out.extend_from_slice(&[0xFF, 0xFE]);
    }
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

/// 按 UTF-16BE 编码；`bom` 决定是否加 FE FF 头
pub fn utf16be(s: &str, bom: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 2 + 2);
    if bom {
        out.extend_from_slice(&[0xFE, 0xFF]);
    }
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_be_bytes());
    }
    out
}
