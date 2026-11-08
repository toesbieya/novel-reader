//! TXT 章节切分。
//!
//! 核心思想：**永远不 split 整本书**。
//! 用 Aho-Corasick 在字节层面预筛"第/Chapter"这些锚点（几万次量级），
//! 只对候选行做解码 + 正则校验，因此耗时与文件大小近似线性且常数极小，
//! 而且拿到的偏移天然就是原文件的字节位置，可以 mmap 直接 seek。

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use aho_corasick::AhoCorasick;
use encoding_rs::Encoding;
use memchr::{memchr, memrchr};
use regex::Regex;

use crate::encoding::{Text, TextError};
use crate::model::ChapterMeta;

/// 章节标题行不会超过这个字节数。中文 GBK 一字 2 字节，200 字节 = 100 字，够宽。
const MAX_TITLE_BYTES: usize = 200;
/// 兜底切块大小（文件里完全没有可识别章节时）
const FALLBACK_BLOCK: usize = 8000;

static TITLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s*(?:第\s*[0-9零〇一二三四五六七八九十百千万两]{1,12}\s*[章节回卷]|(?:[Cc]hapter|CHAPTER)\s*[0-9]{1,4})\s*[^\n]{0,40}$",
    )
    .expect("章节正则写错了")
});

pub struct TxtBook {
    pub(crate) text: Text,
    pub(crate) chapters: Vec<ChapterMeta>,
    pub(crate) spans: Vec<(usize, usize)>,
    pub(crate) title: String,
    pub(crate) path: PathBuf,
}

impl TxtBook {
    pub fn open(path: &Path) -> Result<Self, TextError> {
        let text = Text::open(path)?;
        let (chapters, spans) = split_chapters(&text);
        let title = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("未命名")
            .to_string();

        Ok(TxtBook { text, chapters, spans, title, path: path.to_path_buf() })
    }

    pub fn chapters(&self) -> &[ChapterMeta] {
        &self.chapters
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn encoding_name(&self) -> &'static str {
        self.text.encoding_name()
    }

    pub fn confidence(&self) -> f32 {
        self.text.confidence()
    }

    pub fn chapter_count(&self) -> usize {
        self.chapters.len()
    }

    /// 每章在原文件里的字节区间，导入时落库
    pub fn spans(&self) -> &[(usize, usize)] {
        &self.spans
    }

    /// 只解码这一章的字节区间。30MB 的书读一章的成本和 300KB 的书一样。
    pub fn read_chapter(&self, idx: usize) -> Option<String> {
        let (start, end) = *self.spans.get(idx)?;
        let raw = self.text.decode(start, end);
        Some(raw.trim_start_matches('\u{feff}').trim_start().to_string())
    }

    pub fn file_size(&self) -> u64 {
        self.text.bytes().len() as u64
    }

    /// 命中字节位置 → 章节下标
    pub(crate) fn chapter_at(&self, pos: usize) -> usize {
        let i = self.spans.partition_point(|(s, _)| *s <= pos);
        i.saturating_sub(1)
    }

    pub(crate) fn span_of(&self, idx: usize) -> (usize, usize) {
        self.spans[idx]
    }
}

fn split_chapters(text: &Text) -> (Vec<ChapterMeta>, Vec<(usize, usize)>) {
    let buf = text.bytes();
    let anchors = anchor_patterns(text.encoding());

    let ac = match AhoCorasick::new(&anchors) {
        Ok(ac) => ac,
        Err(_) => return fallback_blocks(buf),
    };

    // (start_byte, title)
    let mut found: Vec<(usize, String)> = Vec::new();
    let mut last_line = usize::MAX;

    for m in ac.find_iter(buf) {
        let pos = m.start();
        let line_start = match memrchr(b'\n', &buf[..pos]) {
            Some(i) => i + 1,
            None => 0,
        };
        if line_start == last_line {
            continue;
        }
        let line_end = match memchr(b'\n', &buf[pos..]) {
            Some(i) => pos + i,
            None => buf.len(),
        };
        // 长度不是标题的直接淘汰，避免解码
        if line_end.saturating_sub(line_start) > MAX_TITLE_BYTES {
            continue;
        }
        last_line = line_start;

        let line = text.decode(line_start, line_end);
        if looks_like_title(&line) {
            if found.last().map(|(s, _)| *s) == Some(line_start) {
                continue;
            }
            found.push((line_start, line.trim().to_string()));
        }
    }

    // 找不到章节 → 按固定块切，保证"能翻能搜"
    if found.len() < 2 {
        return fallback_blocks(buf);
    }

    let mut chapters = Vec::with_capacity(found.len() + 1);
    let mut spans = Vec::with_capacity(found.len() + 1);

    // 第一个章节标题之前的正文（序章 / 版权页）单独成章，但不为几行空白建章
    let first_start = found[0].0;
    if first_start > 0 {
        let head = text.decode(0, first_start);
        if head.trim().chars().count() >= 30 {
            chapters.push(ChapterMeta { index: 0, title: "开始".to_string() });
            spans.push((0, first_start));
        }
    }

    for (i, (start, title)) in found.iter().enumerate() {
        let end = found.get(i + 1).map(|(s, _)| *s).unwrap_or(buf.len());
        if end <= *start {
            continue;
        }
        chapters.push(ChapterMeta { index: chapters.len(), title: title.clone() });
        spans.push((*start, end));
    }

    (chapters, spans)
}

fn fallback_blocks(buf: &[u8]) -> (Vec<ChapterMeta>, Vec<(usize, usize)>) {
    let mut chapters = Vec::new();
    let mut spans = Vec::new();
    let mut start = 0usize;

    while start < buf.len() {
        let target = (start + FALLBACK_BLOCK).min(buf.len());
        let end = if target >= buf.len() {
            buf.len()
        } else {
            match memchr(b'\n', &buf[target..]) {
                Some(i) => target + i + 1,
                None => buf.len(),
            }
        };
        chapters.push(ChapterMeta { index: chapters.len(), title: format!("第 {} 节", chapters.len() + 1) });
        spans.push((start, end));
        start = end;
    }

    (chapters, spans)
}

/// 按文件编码生成锚点字节序列。注意去重：不同编码下 "第" 的字节不同。
fn anchor_patterns(encoding: &'static Encoding) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    for s in ["第", "Chapter", "chapter", "CHAPTER"] {
        let (b, _, _) = encoding.encode(s);
        let v = b.into_owned();
        if !v.is_empty() && !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

fn looks_like_title(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return false;
    }
    if t.chars().count() > 48 {
        return false;
    }
    // 句读符号是正文的铁证。章节名里极少出现句号/逗号/分号。
    if t.contains('。') || t.contains('，') || t.contains('；') {
        return false;
    }
    TITLE_RE.is_match(t)
}
