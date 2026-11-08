//! 全文搜索。
//!
//! TXT 走"查询串按文件编码编码 → 字节子串匹配"，全程 mmap 不拷贝；
//! 匹配速度上限由 memchr 的 SIMD 实现决定，通常 1GB/s 以上。
//! 命中位置换算成"章节 + 章内字符偏移"，前端只需要渲染那一章。

use std::time::Instant;

use memchr::{memchr, memmem, memrchr};

use crate::epub::EpubBook;
use crate::model::{Hit, SearchOutcome};
use crate::txt::TxtBook;

/// 摘要最多多少个字。结果行 CSS 是两行截断（324px 抽屉 / 13px 字 ≈ 两行 46 字），
/// 再长关键字就可能被 clamp 裁掉——这是"预览里看不到命中词"的主要来源。
const SNIPPET_MAX_CHARS: usize = 40;
/// 命中前面保留多少上下文。让关键字稳稳落在第一行，不依赖具体字号 / 抽屉宽度。
const SNIPPET_LEAD: usize = 10;
const SNIPPET_TAIL: usize = SNIPPET_MAX_CHARS - SNIPPET_LEAD;

impl TxtBook {
    pub fn search(&self, query: &str, limit: usize) -> SearchOutcome {
        self.search_paged(query, limit, 0)
    }

    /// 分页搜索：`offset` 之前的命中只计数、不生成 Hit；`total` 始终是全书精确总数。
    ///
    /// 字节 → 字符的扫描游标只前进不回退，整章只扫一遍（旧实现每个命中都
    /// `decode(章首, 命中)`，是 O(章长 × 命中数)）。它同时解决非自同步编码的
    /// **跨字符假命中**：GB18030 / Big5 的查询字节可能横跨两个汉字命中，
    /// 游标没停在 pos 上就说明这个字节位置落在字符中间，必须丢掉；
    /// 否则 `char_offset` 会从字符中间解码，摘要和跳转一起错位。
    pub fn search_paged(&self, query: &str, limit: usize, offset: usize) -> SearchOutcome {
        let t0 = Instant::now();
        let q = query.trim();
        if q.is_empty() {
            return SearchOutcome::empty(0);
        }

        let needle = self.text.encode_query(q);
        if needle.is_empty() {
            return SearchOutcome::empty(t0.elapsed().as_micros());
        }

        let buf = self.text.bytes();
        let finder = memmem::Finder::new(&needle);
        let mut hits = Vec::new();
        let mut total = 0usize;

        let mut scan_byte = 0usize;
        let mut scan_chars = 0usize;
        let mut scan_chapter = usize::MAX;

        for pos in finder.find_iter(buf) {
            // memmem 的命中不重叠，但跳过一个跨界假命中后游标可能已越过下一个命中
            if pos < scan_byte {
                continue;
            }
            let ci = self.chapter_at(pos);
            if ci != scan_chapter {
                scan_chapter = ci;
                scan_byte = self.span_of(ci).0;
                scan_chars = 0;
            }
            while scan_byte < pos {
                scan_byte += self.text.char_len_at(scan_byte);
                scan_chars += 1;
            }
            if scan_byte != pos {
                continue; // 落在多字节字符中间：假命中，不计入 total
            }

            total += 1;
            if total <= offset || hits.len() >= limit {
                continue;
            }

            hits.push(Hit {
                chapter_index: ci,
                chapter_title: self.chapters[ci].title.clone(),
                char_offset: scan_chars,
                snippet: self.snippet_at(pos, q),
            });
        }

        SearchOutcome { hits, total, elapsed_us: t0.elapsed().as_micros() }
    }

    /// 以命中所在的整行为单位取摘要——小说一行本来就不长，比固定窗口自然。
    ///
    /// 命中位置必须由**本次命中的字节偏移**换算，不能用 `line.find(q)`：
    /// 同一行里出现多次时 find 永远返回第一处，所有结果行会显示同一段摘要，
    /// 靠后的命中更会对不上自己那一处。
    fn snippet_at(&self, pos: usize, q: &str) -> String {
        let buf = self.text.bytes();
        let line_start = memrchr(b'\n', &buf[..pos]).map(|i| i + 1).unwrap_or(0);
        let line_end = memchr(b'\n', &buf[pos..]).map(|i| pos + i).unwrap_or(buf.len());
        let raw = self.text.decode(line_start, line_end);
        let hit_char = self.text.char_count(line_start, pos);
        // trim 会吃掉行首空白，命中下标要同步前移，否则窗口整体偏后
        let trimmed_lead = raw.chars().take_while(|c| c.is_whitespace()).count();
        snippet_around(raw.trim(), hit_char.saturating_sub(trimmed_lead), q.chars().count())
    }
}

impl EpubBook {
    pub fn search(&self, query: &str, limit: usize) -> SearchOutcome {
        self.search_paged(query, limit, 0)
    }

    pub fn search_paged(&self, query: &str, limit: usize, offset: usize) -> SearchOutcome {
        let t0 = Instant::now();
        let q = query.trim();
        if q.is_empty() {
            return SearchOutcome::empty(0);
        }

        let mut hits = Vec::new();
        let mut total = 0usize;

        // 第一次搜索会把整本解压成纯文本并落盘（搜索专用，不用于显示）；
        // 之后（包括重启应用后）直接读缓存，省掉每次搜索的逐章解压。
        // 扫描用 memmem（SIMD），和 TXT 搜索同一套字节匹配。
        let finder = memmem::Finder::new(q.as_bytes());
        let texts = self.texts_for_search();
        for (idx, text) in texts.iter().enumerate() {
            let meta = &self.chapters[idx];
            // 合法 UTF-8 查询串只可能命中字符边界；相邻命中之间增量数字符，
            // 整章只扫一遍，不用每次 text[..b].chars().count() 从头数。
            let mut scan_byte = 0usize;
            let mut scan_chars = 0usize;

            for b in finder.find_iter(text.as_bytes()) {
                scan_chars += text[scan_byte..b].chars().count();
                scan_byte = b;
                total += 1;
                if total <= offset || hits.len() >= limit {
                    continue;
                }
                hits.push(Hit {
                    chapter_index: idx,
                    chapter_title: meta.title.clone(),
                    char_offset: scan_chars,
                    snippet: snippet_utf8(text, b, q),
                });
            }
        }

        SearchOutcome { hits, total, elapsed_us: t0.elapsed().as_micros() }
    }
}

/// 字符版（TXT）：以本次命中为中心截一段，关键字完整保留且靠前。
/// `hit_char` 是命中在 line 里的字符下标，`q_chars` 是查询词字符数。
fn snippet_around(line: &str, hit_char: usize, q_chars: usize) -> String {
    let from = hit_char.saturating_sub(SNIPPET_LEAD);
    // 查询词比窗口还长时宁可撑长摘要，也不能把关键字切掉一半
    let to = (from + SNIPPET_MAX_CHARS).max(hit_char + q_chars);

    let mut s = String::new();
    let mut more_after = false;
    for (i, ch) in line.chars().enumerate() {
        if i < from {
            continue;
        }
        if i >= to {
            more_after = true;
            break;
        }
        s.push(if ch == '\n' || ch == '\r' { ' ' } else { ch });
    }
    if from > 0 {
        s.insert(0, '…');
    }
    if more_after {
        s.push('…');
    }
    s
}

/// 字节版（EPUB）：一章常常就是一行几万字，不能整行解码 / 整行 find。
/// 只往命中的前后各看固定个字符，用 `char_indices` 正反各走固定步数拿字节边界，
/// 代价与章长无关，比旧实现还省。
fn snippet_utf8(text: &str, hit: usize, q: &str) -> String {
    // 命中处一定是字符边界（q 是合法 UTF-8，memmem 只匹配完整序列）
    let start = text[..hit]
        .char_indices()
        .rev()
        .take(SNIPPET_LEAD)
        .last() // 倒数第 LEAD 个字符的起点；用 nth(LEAD) 会多带一个字符
        .map(|(i, _)| i)
        .unwrap_or(0);
    let tail_at = hit + q.len();
    let end = text[tail_at..]
        .char_indices()
        .nth(SNIPPET_TAIL)
        .map(|(i, _)| tail_at + i)
        .unwrap_or(text.len());

    let mut s = String::new();
    if start > 0 {
        s.push('…');
    }
    for ch in text[start..end].chars() {
        s.push(if ch == '\n' || ch == '\r' { ' ' } else { ch });
    }
    if end < text.len() {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_around_keeps_hit_short_and_near_front() {
        let line = format!("{}雪{}", "甲".repeat(60), "乙".repeat(60));
        let s = snippet_around(&line, 60, 1);
        assert!(s.contains('雪'));
        assert!(s.chars().count() <= SNIPPET_MAX_CHARS + 2, "摘要过长：{s}");
        // 关键字要在第一行；开头的「…」占 1 个字符
        assert!(s.chars().position(|c| c == '雪').unwrap() <= SNIPPET_LEAD + 1, "{s}");
    }

    #[test]
    fn snippet_around_short_line_still_centers_on_hit() {
        let line = format!("{}雪", "甲".repeat(20));
        let s = snippet_around(&line, 20, 1);
        assert!(s.contains('雪'));
        assert!(s.starts_with('…'), "命中靠后时前面要有截断标记：{s}");
        assert!(s.ends_with('雪'));
    }

    #[test]
    fn snippet_around_never_splits_a_long_query() {
        let q = "雪落山门".repeat(12); // 48 字，比窗口还长
        let line = format!("{}{}", "甲".repeat(20), q);
        let s = snippet_around(&line, 20, q.chars().count());
        assert!(s.contains(&q), "超长查询词也必须完整出现：{s}");
    }

    #[test]
    fn snippet_utf8_keeps_hit_visible_without_scanning_the_chapter() {
        let prefix = "甲".repeat(20_000);
        let hit = prefix.len();
        let text = format!("{prefix}雪{}", "乙".repeat(20_000));
        let s = snippet_utf8(&text, hit, "雪");
        assert!(s.contains('雪'));
        assert!(s.chars().position(|c| c == '雪').unwrap() <= SNIPPET_LEAD + 1, "{s}");
        assert!(s.chars().count() <= SNIPPET_MAX_CHARS + 3, "摘要过长：{s}");
    }

    #[test]
    fn snippet_utf8_handles_hit_at_text_start_and_end() {
        assert!(snippet_utf8("雪", 0, "雪").contains('雪'));
        let s = snippet_utf8("前面还有一些字雪", "前面还有一些字".len(), "雪");
        assert!(s.contains('雪'));
        assert!(s.ends_with('雪'));
    }
}
