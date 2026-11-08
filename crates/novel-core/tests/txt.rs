//! 章节切分的行为测试。
//! 全部从 `TxtBook::open` 这个公开入口验——盯的是"用户看到的目录和正文"，
//! 不依赖内部函数签名，重构切分实现时这些测试不该跟着改。

mod common;

use common::{gb18030, utf16le, TempTxt};
use novel_core::TxtBook;

/// 把 `content` 按 UTF-8 写盘并打开；返回的 `TempTxt` 要一直持有，析构即删文件
fn open_text(name: &str, content: &str) -> (TxtBook, TempTxt) {
    open_bytes(name, content.as_bytes())
}

/// 原样写字节（编码由调用方决定）后打开
fn open_bytes(name: &str, bytes: &[u8]) -> (TxtBook, TempTxt) {
    let file = TempTxt::new(name, bytes);
    let book = TxtBook::open(file.path()).expect("打开测试文本");
    (book, file)
}

/// 目录标题列表
fn titles(book: &TxtBook) -> Vec<&str> {
    book.chapters().iter().map(|c| c.title.as_str()).collect()
}

const BOOK: &str = "第一章 起点\n正文一。\n第二章 中段\n正文二。\n第三章 终点\n正文三。\n";

#[test]
fn splits_chapters_and_keeps_byte_spans() {
    let (book, _file) = open_text("basic", BOOK);
    assert_eq!(titles(&book), ["第一章 起点", "第二章 中段", "第三章 终点"]);
    assert_eq!(book.read_chapter(0).unwrap(), "第一章 起点\n正文一。\n");

    // 偏移必须首尾相接且覆盖到文件末尾：阅读全靠它 seek
    let spans = book.spans();
    assert_eq!(spans[0].0, 0);
    for pair in spans.windows(2) {
        assert_eq!(pair[0].1, pair[1].0);
    }
    assert_eq!(spans.last().unwrap().1, book.file_size() as usize);
    assert!(book.read_chapter(3).is_none(), "越界章节返回 None");
}

#[test]
fn long_preamble_becomes_start_chapter() {
    let preamble = "序".repeat(40);
    let content = format!("{preamble}\n第一章 甲\n正文A。\n第二章 乙\n正文B。\n");
    let (book, _file) = open_text("preamble", &content);
    assert_eq!(titles(&book), ["开始", "第一章 甲", "第二章 乙"]);
    assert_eq!(book.read_chapter(0).unwrap().trim_end(), preamble);
}

#[test]
fn short_preamble_is_dropped() {
    let (book, _file) = open_text("short-head", "序\n第一章 甲\n正文A。\n第二章 乙\n正文B。\n");
    assert_eq!(titles(&book), ["第一章 甲", "第二章 乙"]);
    assert_eq!(book.spans()[0].0, "序\n".len(), "短前言不建章，但也不能算进第一章");
}

#[test]
fn mixed_crlf_and_lf_titles_are_trimmed() {
    let (book, _file) =
        open_text("crlf", "第一章 甲\r\n正文A。\r\n第二章 乙\n正文B。\n");
    assert_eq!(titles(&book), ["第一章 甲", "第二章 乙"]);
    assert!(book.read_chapter(0).unwrap().contains("\r\n"), "正文保留原始换行");
}

#[test]
fn chapter_without_body_is_kept() {
    let (book, _file) = open_text("title-only", "第一章 甲\n第二章 乙\n");
    assert_eq!(titles(&book), ["第一章 甲", "第二章 乙"]);
    assert_eq!(book.read_chapter(1).unwrap().trim(), "第二章 乙");
}

#[test]
fn overlong_line_is_not_a_title() {
    let long = format!("第二章 {}", "字".repeat(120));
    let content = format!("第一章 甲\n{long}\n第三章 丙\n正文。\n");
    let (book, _file) = open_text("long-line", &content);
    assert_eq!(titles(&book), ["第一章 甲", "第三章 丙"]);
    assert!(book.read_chapter(0).unwrap().contains("第二章 字"), "超长行留在正文里");
}

#[test]
fn prose_mentioning_chapters_does_not_split() {
    let content = "第一章 甲\n\
他翻到第十章的头发时，愣了一下。\n\
第十章的头发很长。\n\
第一篇文章说得好\n\
第二章 乙\n正文。\n";
    let (book, _file) = open_text("false-positive", content);
    assert_eq!(titles(&book), ["第一章 甲", "第二章 乙"]);
}

#[test]
fn single_title_falls_back_to_blocks() {
    let (book, _file) = open_text("single", "第一章 独苗\n正文。\n");
    assert_eq!(titles(&book), ["第 1 节"], "认出的章节不足 2 个时走兜底切块");
}

#[test]
fn one_huge_line_is_one_chapter() {
    let content = "雪".repeat(20_000);
    let (book, _file) = open_text("huge-line", &content);
    assert_eq!(book.chapter_count(), 1, "没有换行可切时不能硬切字节");
    let body = book.read_chapter(0).unwrap();
    assert_eq!(body.chars().count(), 20_000);
    assert!(!body.contains('\u{FFFD}'), "兜底切块不能切断多字节字符");
}

#[test]
fn utf16le_without_bom_is_transcoded_then_split() {
    let content = "第一章 甲\n\
他推开门，风裹着雪灌进来，灯芯晃了一下，院子里积了半尺厚的雪。\n\
第二章 乙\n\
天蒙蒙亮的时候，长街上的铺子还没开门，卖炭的老人推着车从巷口经过。\n";
    assert!(content.chars().count() >= 64, "UTF-16 嗅探至少要 128 字节");
    let (book, _file) = open_bytes("utf16", &utf16le(content, false));
    assert_eq!(book.encoding_name(), "UTF-8", "UTF-16 打开时已整体转码");
    assert_eq!(titles(&book), ["第一章 甲", "第二章 乙"]);
    assert!(book.read_chapter(0).unwrap().contains("灯芯晃了一下"));
}

#[test]
fn gb18030_chapter_boundaries_are_clean() {
    let content = "第一章 雪夜\n\
他推开门，风裹着雪灌进来。\n\
第二章 长街\n\
卖炭的老人推着车从巷口经过。\n\
第三章 山口\n\
雪粒打在脸上，像细小的针。\n";
    let (book, _file) = open_bytes("gb18030", &gb18030(content));
    assert_eq!(book.encoding_name(), "gb18030");
    assert_eq!(titles(&book), ["第一章 雪夜", "第二章 长街", "第三章 山口"]);
    for i in 0..book.chapter_count() {
        let body = book.read_chapter(i).unwrap();
        assert!(!body.contains('\u{FFFD}'), "第 {i} 章切在了字符边界上");
        assert!(!body.trim().is_empty());
    }
}

#[test]
fn multi_character_numerals_are_recognized() {
    // 真实书上千章，标题里是"第一百零八章"这种多字数字，不是只有"第一章"。
    let content = "第一章 甲\n正文。\n第十二章 乙\n正文。\n第一百零八章 丙\n正文。\n";
    let (book, _file) = open_text("numerals", content);
    assert_eq!(titles(&book), ["第一章 甲", "第十二章 乙", "第一百零八章 丙"]);
}

#[test]
fn english_chapter_headings_are_recognized() {
    let (book, _file) = open_text("english", "Chapter 1\nHello.\nChapter 2\nWorld.\n");
    assert_eq!(titles(&book), ["Chapter 1", "Chapter 2"]);
}

#[test]
fn uppercase_chapter_headings_are_recognized() {
    // 正则收 CHAPTER，但 Aho-Corasick 预筛锚点里没有它 → 整行永远进不了候选，漏章
    let (book, _file) = open_text("upper-english", "CHAPTER 1\nHello.\nCHAPTER 2\nWorld.\n");
    assert_eq!(titles(&book), ["CHAPTER 1", "CHAPTER 2"]);
}

// ---------------------------------------------------------------- 搜索摘要

#[test]
fn search_snippet_always_shows_the_query() {
    // 旧实现：行长 ≤ 80 字时整行返回，关键字落在末尾会被结果行的两行 clamp 裁掉
    let body = format!("{}雪。", "甲".repeat(60));
    let content = format!("第一章 甲\n{body}\n第二章 乙\n正文雪。\n");
    let (book, _file) = open_text("snippet-tail", &content);
    let out = book.search("雪", 10);
    assert!(out.total >= 2, "两处命中都要算");
    for h in &out.hits {
        assert!(h.snippet.contains('雪'), "摘要必须含关键字：{}", h.snippet);
        assert!(h.snippet.chars().count() <= 42, "摘要不能长到被两行裁掉：{}", h.snippet);
        let at = h.snippet.chars().position(|c| c == '雪').unwrap();
        assert!(at <= 12, "关键字要落在第一行：{}", h.snippet);
    }
}

#[test]
fn same_line_hits_get_their_own_snippets() {
    // 旧实现用 find(q) 取第一处，同一行的所有结果摘要一模一样
    let line = format!("{}雪{}雪{}雪", "甲".repeat(25), "乙".repeat(25), "丙".repeat(25));
    let content = format!("第一章 甲\n{line}\n第二章 乙\n正文。\n");
    let (book, _file) = open_text("snippet-multi", &content);
    let out = book.search("雪", 10);
    assert_eq!(out.total, 3);
    let snips: Vec<&str> = out.hits.iter().map(|h| h.snippet.as_str()).collect();
    assert_ne!(snips[0], snips[1], "每条命中要围绕自己那一处");
    assert_ne!(snips[1], snips[2]);
    for s in &snips {
        assert!(s.contains('雪'));
    }
}

// ---------------------------------------------------------------- 搜索边界与分页

#[test]
fn gb18030_search_rejects_cross_character_hits() {
    // 找一个 GB18030 双字节汉字 q=[a,b]，且 b 能当下一个汉字的 lead（≥0x81）。
    // 字节流 [B0, a, b, D0] 是两个汉字 X=[B0,a]、Y=[b,D0]，
    // 但**跨字符**位置恰好出现 q 的编码 [a,b]：旧实现会把它算成命中，
    // 并从字符中间 decode 出偏移错乱、摘要错位的假结果。
    let mut query = None;
    for code in 0x4E00u32..=0x9FFF {
        let ch = char::from_u32(code).unwrap();
        let b = gb18030(&ch.to_string());
        if b.len() == 2 && b[0] >= 0x81 && (0x81..=0xFE).contains(&b[1]) {
            query = Some((ch, b));
            break;
        }
    }
    let (q, qb) = query.expect("要能找到合适的汉字");

    // 重复 20 次保证编码探测稳定；末尾再放一个**边界对齐**的 q 当真命中。
    // 不用「第一章」这类中文标题，免得 q 恰好是标题里的字、真命中混进计数。
    let mut bytes = Vec::new();
    for _ in 0..20 {
        bytes.extend_from_slice(&[0xB0, qb[0], qb[1], 0xD0]);
    }
    bytes.extend_from_slice(&qb);
    bytes.push(b'\n');
    let (book, _file) = open_bytes("gbk-cross", &bytes);
    assert_eq!(book.encoding_name(), "gb18030", "语料必须按 GB18030 探测");
    let out = book.search(&q.to_string(), 10);
    assert_eq!(out.total, 1, "20 个跨界假命中都要排除，只留末尾那处真命中");
    assert_eq!(out.hits[0].char_offset, 40, "20 个模式 = 40 个字符，偏移按字符算");
}

#[test]
fn search_paged_offset_keeps_precise_total() {
    let content = format!("第一章 甲\n{}\n", "雪".repeat(7));
    let (book, _file) = open_text("paged", &content);
    assert_eq!(book.search("雪", 100).total, 7);

    let first = book.search_paged("雪", 2, 0);
    let later = book.search_paged("雪", 2, 4);
    assert_eq!(first.total, 7, "total 与 offset 无关");
    assert_eq!(later.total, 7);
    assert_eq!(first.hits.len(), 2);
    assert_eq!(later.hits.len(), 2);
    assert_eq!(first.hits[1].char_offset - first.hits[0].char_offset, 1);
    assert_eq!(later.hits[1].char_offset - later.hits[0].char_offset, 1);
    assert_eq!(later.hits[0].char_offset - first.hits[0].char_offset, 4);
}
