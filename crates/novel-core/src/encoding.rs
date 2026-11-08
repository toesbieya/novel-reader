//! 编码探测与按区间解码。
//!
//! 中文网文现实里 GBK / GB18030 占比很高，纯 UTF-8 假设会直接读成乱码。
//! 策略：BOM → UTF-16 嗅探 → UTF-8 全量校验 → GB18030 / Big5 采样打分。

use std::fs::File;
use std::path::Path;

use encoding_rs::{Encoding, BIG5, GB18030, UTF_16BE, UTF_16LE, UTF_8};
use memmap2::Mmap;
use thiserror::Error;

/// 打分采样窗口大小。1MB 足以分辨中文编码，又不影响导入耗时。
const SAMPLE_LIMIT: usize = 1 << 20;

#[derive(Debug, Error)]
pub enum TextError {
    #[error("打不开文件：{0}")]
    Open(String),
    #[error("文件是空的：{0}")]
    Empty(String),
    #[error("内存映射失败：{0}")]
    Map(String),
}

#[derive(Debug, Clone, Copy)]
pub struct Detection {
    pub encoding: &'static Encoding,
    pub bom_len: usize,
    /// 0..1，UTF-8 直判为 1.0，采样打分为归一化置信度
    pub confidence: f32,
}

pub fn detect(buf: &[u8]) -> Detection {
    if let Some((enc, len)) = Encoding::for_bom(buf) {
        return Detection { encoding: enc, bom_len: len, confidence: 1.0 };
    }

    if let Some(enc) = sniff_utf16(buf) {
        return Detection { encoding: enc, bom_len: 0, confidence: 0.9 };
    }

    if std::str::from_utf8(buf).is_ok() {
        return Detection { encoding: UTF_8, bom_len: 0, confidence: 1.0 };
    }

    let sample = sample_window(buf);
    let mut best = (GB18030, f32::MIN);
    for enc in [GB18030, BIG5] {
        let s = score(enc, &sample);
        if s > best.1 {
            best = (enc, s);
        }
    }

    // 把原始分（约 0..100）压到 0..1
    Detection { encoding: best.0, bom_len: 0, confidence: (best.1 / 100.0).clamp(0.0, 1.0) }
}

/// 无 BOM 的 UTF-16 嗅探。
///
/// 两个信号并行判断，缺一不可：
/// - **零字节比例**：英文/数字为主的 UTF-16，高位字节恒为 0
/// - **CJK 高位字节比例**：中文 UTF-16 的高位字节落在 0x4E..=0x9F，
///   这是纯零字节判断抓不到的（汉字高位永远不是 0）
///
/// 阈值 0.45：GBK 字节流落在 0x4E..=0x9F 的概率约 0.32，不会误判。
fn sniff_utf16(buf: &[u8]) -> Option<&'static Encoding> {
    let n = buf.len().min(8192) & !1;
    if n < 128 {
        return None;
    }

    let mut even_zero = 0usize;
    let mut odd_zero = 0usize;
    let mut even_cjk = 0usize;
    let mut odd_cjk = 0usize;

    for i in (0..n).step_by(2) {
        let (lo, hi) = (buf[i], buf[i + 1]);
        if lo == 0 {
            even_zero += 1;
        }
        if hi == 0 {
            odd_zero += 1;
        }
        if (0x4E..=0x9F).contains(&lo) {
            even_cjk += 1;
        }
        if (0x4E..=0x9F).contains(&hi) {
            odd_cjk += 1;
        }
    }

    let half = (n / 2) as f32;
    let le = (odd_zero as f32 / half).max(odd_cjk as f32 / half);
    let be = (even_zero as f32 / half).max(even_cjk as f32 / half);

    const THRESHOLD: f32 = 0.45;

    if le >= THRESHOLD && le >= be {
        return Some(UTF_16LE);
    }
    if be >= THRESHOLD && be > le {
        return Some(UTF_16BE);
    }
    None
}

/// 取开头 + 中段拼接，避免文件头部是一大段纯英文元数据导致误判。
/// 拼接处可能切断一个多字节字符，但只影响 1 个 U+FFFD，对打分无实质影响。
fn sample_window(buf: &[u8]) -> Vec<u8> {
    if buf.len() <= SAMPLE_LIMIT {
        return buf.to_vec();
    }
    let half = SAMPLE_LIMIT / 2;
    let mid = buf.len() / 2;
    let mut out = Vec::with_capacity(SAMPLE_LIMIT);
    out.extend_from_slice(&buf[..half]);
    out.extend_from_slice(&buf[mid..mid + half]);
    out
}

fn score(enc: &'static Encoding, sample: &[u8]) -> f32 {
    let (text, _, _) = enc.decode(sample);
    let mut cjk = 0usize;
    let mut bad = 0usize;
    let mut total = 0usize;

    for ch in text.chars() {
        total += 1;
        let c = ch as u32;
        if ch == '\u{FFFD}' {
            bad += 1;
        } else if (0x4E00..=0x9FFF).contains(&c)
            || (0x3000..=0x303F).contains(&c)
            || (0xFF00..=0xFFEF).contains(&c)
        {
            cjk += 1;
        }
    }

    if total == 0 {
        return f32::MIN;
    }
    let total_f = total as f32;
    (cjk as f32 / total_f) * 100.0 - (bad as f32 / total_f) * 500.0
}

enum Data {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

/// 一本书的字节载体。UTF-16 会在打开时整体转码为 UTF-8 常驻内存
/// （中文 epub/txt 里 UTF-16 极少见，这个代价换来 offsets 语义统一）。
pub struct Text {
    data: Data,
    origin: usize,
    encoding: &'static Encoding,
    confidence: f32,
}

impl Text {
    pub fn open(path: &Path) -> Result<Self, TextError> {
        let file = File::open(path).map_err(|e| TextError::Open(format!("{} ({e})", path.display())))?;
        let meta = file.metadata().map_err(|e| TextError::Open(e.to_string()))?;
        if meta.len() == 0 {
            return Err(TextError::Empty(path.display().to_string()));
        }

        // SAFETY: 只读映射；文件被外部改写时由书架层的 mtime 校验兜底。
        let mmap = unsafe { Mmap::map(&file) }.map_err(|e| TextError::Map(e.to_string()))?;
        let det = detect(&mmap);

        if det.encoding == UTF_16LE || det.encoding == UTF_16BE {
            let (cow, _, _) = det.encoding.decode(&mmap);
            return Ok(Text {
                data: Data::Owned(cow.into_owned().into_bytes()),
                origin: 0,
                encoding: UTF_8,
                confidence: det.confidence,
            });
        }

        Ok(Text {
            data: Data::Mapped(mmap),
            origin: det.bom_len,
            encoding: det.encoding,
            confidence: det.confidence,
        })
    }

    /// 已剥离 BOM 的原始字节。所有章节偏移都是相对这个切片的下标。
    #[inline]
    pub fn bytes(&self) -> &[u8] {
        match &self.data {
            Data::Mapped(m) => &m[self.origin..],
            Data::Owned(v) => &v[self.origin..],
        }
    }

    #[inline]
    pub fn encoding(&self) -> &'static Encoding {
        self.encoding
    }

    pub fn encoding_name(&self) -> &'static str {
        self.encoding.name()
    }

    pub fn confidence(&self) -> f32 {
        self.confidence
    }

    /// 解码字节区间。调用方必须保证 start/end 落在字符边界上
    /// （章节切分总是在 '\n' 之后，天然满足）。
    pub fn decode(&self, start: usize, end: usize) -> String {
        let b = self.bytes();
        let start = start.min(b.len());
        let end = end.min(b.len()).max(start);
        let (cow, _) = self.encoding.decode_without_bom_handling(&b[start..end]);
        cow.into_owned()
    }

    /// 某个字节位置开始的字符占几个字节。
    ///
    /// 搜索时用它把「字节命中位置」推进成「字符边界」：GB18030 / Big5 不是自同步编码，
    /// 查询串的字节序列可能横跨两个汉字命中，只有从已知边界逐字符走过去才能识别。
    /// UTF-16 在 `open` 时已整体转成 UTF-8，所以这里只需处理三种编码。
    #[inline]
    pub fn char_len_at(&self, pos: usize) -> usize {
        let b = self.bytes();
        let b0 = match b.get(pos) {
            Some(&x) => x,
            None => return 1,
        };
        if self.encoding == UTF_8 {
            return match b0 {
                0x00..=0x7F => 1,
                0xC0..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF7 => 4,
                _ => 1, // 非法首字节也按 1 走，避免扫描游标卡死
            };
        }
        if self.encoding == GB18030 {
            if b0 < 0x80 {
                return 1;
            }
            // 四字节字符的第二字节固定是 0x30..=0x39
            if matches!(b.get(pos + 1), Some(0x30..=0x39)) {
                return 4;
            }
            return 2;
        }
        if self.encoding == BIG5 {
            return if b0 < 0x80 { 1 } else { 2 };
        }
        1
    }

    /// 统计 [start, end) 里的字符数。调用方保证两端是字符边界。
    #[inline]
    pub fn char_count(&self, start: usize, end: usize) -> usize {
        let b = self.bytes();
        let start = start.min(b.len());
        let end = end.min(b.len()).max(start);
        if start == end {
            return 0;
        }
        if self.encoding == UTF_8 {
            // 省掉 decode 的复制：UTF-8 合法时直接借切片数字符
            if let Ok(s) = std::str::from_utf8(&b[start..end]) {
                return s.chars().count();
            }
        }
        self.decode(start, end).chars().count()
    }

    /// 把查询串编码成文件自身的字节序列，这样搜索可以直接做字节匹配，
    /// 省掉"整本转码成 UTF-8"这一趟。
    pub fn encode_query(&self, q: &str) -> Vec<u8> {
        let (cow, _, _) = self.encoding.encode(q);
        cow.into_owned()
    }
}

/// 尽力而为地解码一小段字节，用于嗅探作者、书名这类元信息。
/// 不做置信度判断，错了也只是元信息难看，不影响正文。
pub fn decode_loose(bytes: &[u8]) -> String {
    if let Some((enc, _)) = Encoding::for_bom(bytes) {
        return enc.decode_without_bom_handling(bytes).0.into_owned();
    }
    if std::str::from_utf8(bytes).is_ok() {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    GB18030.decode_without_bom_handling(bytes).0.into_owned()
}
