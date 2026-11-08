//! 编码探测与按区间解码的行为测试。
//! 判定口径：别人拿真实小说跑出来是什么样，测试就钉住什么样。

mod common;

use common::{gb18030, utf16be, utf16le, TempTxt};
use encoding_rs::GB18030;
use novel_core::encoding::{decode_loose, detect, Text, TextError};

/// 中文样本，长度必须够 UTF-16 嗅探的 128 字节门槛。
const SAMPLE: &str = "第一章 雪夜\n\
他推开门，风裹着雪灌进来，灯芯晃了一下。\n\
院子里积了半尺厚的雪，脚印一路延伸到大门口。\n\
他站在那里，很久没有说话，只是望着远处的山影。\n\
第二章 长街\n\
天蒙蒙亮的时候，长街上的铺子还没开门。\n\
卖炭的老人推着车从巷口经过，车辙压出两道深痕。\n\
他买了两个热腾腾的馒头，揣在怀里，继续往北走。\n\
第三章 山口\n\
风更大了。他把领口拉紧，一步一步地往上爬。\n\
雪粒打在脸上，像细小的针。\n";

#[test]
fn bom_has_priority_and_bom_len_is_reported() {
    let mut utf8_bom = vec![0xEF, 0xBB, 0xBF];
    utf8_bom.extend_from_slice(SAMPLE.as_bytes());
    let d = detect(&utf8_bom);
    assert_eq!(d.encoding.name(), "UTF-8");
    assert_eq!(d.bom_len, 3);
    assert_eq!(d.confidence, 1.0);

    let d = detect(&utf16le(SAMPLE, true));
    assert_eq!(d.encoding.name(), "UTF-16LE");
    assert_eq!(d.bom_len, 2);

    let d = detect(&utf16be(SAMPLE, true));
    assert_eq!(d.encoding.name(), "UTF-16BE");
    assert_eq!(d.bom_len, 2);
}

#[test]
fn plain_utf8_is_detected_with_full_confidence() {
    let d = detect(SAMPLE.as_bytes());
    assert_eq!(d.encoding.name(), "UTF-8");
    assert_eq!(d.bom_len, 0);
    assert_eq!(d.confidence, 1.0);
}

#[test]
fn gb18030_is_recognized() {
    let d = detect(&gb18030(SAMPLE));
    assert_eq!(d.encoding.name(), "gb18030");
    assert_eq!(d.bom_len, 0);
    assert!(d.confidence > 0.5, "GB18030 的 CJK 得分应明显占优");
}

#[test]
fn utf16_without_bom_is_sniffed() {
    let d = detect(&utf16le(SAMPLE, false));
    assert_eq!(d.encoding.name(), "UTF-16LE");
    assert_eq!(d.bom_len, 0);
    assert_eq!(d.confidence, 0.9);

    let d = detect(&utf16be(SAMPLE, false));
    assert_eq!(d.encoding.name(), "UTF-16BE");
}

#[test]
fn sample_window_cutting_a_char_does_not_break_detection() {
    // 前置一个 ASCII 字节，让 512KB 采样边界正好落在一个 GB18030 双字节字符中间，
    // 验证拼接处的 U+FFFD 不会把打分带偏。
    let mut bytes = vec![b'A'];
    while bytes.len() < 1_200_000 {
        bytes.extend_from_slice(&GB18030.encode("雪夜长街风雪归人").0);
    }
    let d = detect(&bytes);
    assert_eq!(d.encoding.name(), "gb18030");
    assert!(d.confidence > 0.5);
}

#[test]
fn text_open_transcodes_utf16_to_utf8() {
    let file = TempTxt::new("utf16le", &utf16le(SAMPLE, false));
    let text = Text::open(file.path()).expect("打开 UTF-16LE");
    assert_eq!(text.encoding_name(), "UTF-8");
    assert_eq!(text.bytes(), SAMPLE.as_bytes(), "转码后偏移语义统一为 UTF-8");
    assert_eq!(text.decode(0, text.bytes().len()), SAMPLE);
}

#[test]
fn text_open_strips_bom_from_offsets() {
    let mut with_bom = vec![0xEF, 0xBB, 0xBF];
    with_bom.extend_from_slice(SAMPLE.as_bytes());
    let file = TempTxt::new("utf8bom", &with_bom);
    let text = Text::open(file.path()).expect("打开 UTF-8 BOM");
    assert_eq!(text.bytes(), SAMPLE.as_bytes(), "BOM 不计入章节偏移");
    assert_eq!(text.decode(0, 6), "第一");

    let file = TempTxt::new("utf16bom", &utf16le(SAMPLE, true));
    let text = Text::open(file.path()).expect("打开 UTF-16LE BOM");
    assert_eq!(text.bytes(), SAMPLE.as_bytes(), "UTF-16 的 BOM 在转码时剥掉");
}

#[test]
fn text_open_rejects_empty_file() {
    let file = TempTxt::new("empty", b"");
    assert!(matches!(Text::open(file.path()), Err(TextError::Empty(_))));
}

#[test]
fn decode_clamps_to_file_bounds() {
    let file = TempTxt::new("ascii", b"abcdef");
    let text = Text::open(file.path()).expect("打开 ASCII");
    assert_eq!(text.decode(2, 4), "cd");
    assert_eq!(text.decode(0, 9999), "abcdef", "end 超界按文件长度截断");
    assert_eq!(text.decode(100, 200), "", "start 超界解出空串");
    assert_eq!(text.decode(4, 2), "", "end < start 解出空串");
}

#[test]
fn encode_query_uses_file_encoding() {
    // 搜索是拿原文件的字节做 memmem，查询串必须按同一编码编码。
    let file = TempTxt::new("gb-query", &gb18030(SAMPLE));
    let text = Text::open(file.path()).expect("打开 GB18030");
    assert_eq!(text.encode_query("雪夜"), gb18030("雪夜"));

    let file = TempTxt::new("utf8-query", SAMPLE.as_bytes());
    let text = Text::open(file.path()).expect("打开 UTF-8");
    assert_eq!(text.encode_query("雪夜"), "雪夜".as_bytes());
}

#[test]
fn decode_loose_falls_back_to_gb18030() {
    assert_eq!(decode_loose(SAMPLE.as_bytes()), SAMPLE);
    assert_eq!(decode_loose(&gb18030("雪夜长街")), "雪夜长街");
}
