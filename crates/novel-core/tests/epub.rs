//! EPUB 搜索与纯文本缓存的行为。
//! 缓存是可选加速项：无论走内存、磁盘缓存还是现解压，结果必须一致。

use std::path::PathBuf;

use novel_core::EpubBook;

/// 仓库里的样本 EPUB 路径（测试的工作目录是 crate 根）
fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples").join(name)
}

#[test]
fn metadata_author_comes_from_opf() {
    let path = sample("test_epub3.epub");
    let book = EpubBook::open(&path).expect("打开样本 EPUB");
    assert_eq!(book.title(), "雪落山门");
    assert_eq!(book.author(), Some("测试作者"));
}

#[test]
fn search_results_survive_memory_and_disk_cache() {
    let path = sample("test_epub3.epub");
    let book = EpubBook::open(&path).expect("打开样本 EPUB");

    let first = book.search("雪", 100);
    assert!(first.total > 0, "样本里应该能搜到「雪」");
    assert!(
        first.hits.iter().all(|h| h.snippet.contains('雪')),
        "每一条结果行的预览都必须看得到命中词"
    );

    // 同一次打开：走内存缓存，结果不能变
    let second = book.search("雪", 100);
    assert_eq!(second.total, first.total);
    assert_eq!(second.hits.len(), first.hits.len());

    // 重新打开：能读到磁盘缓存；结果仍必须一致
    let reopened = EpubBook::open(&path).expect("重新打开样本 EPUB");
    let third = reopened.search("雪", 100);
    assert_eq!(third.total, first.total);
    assert_eq!(third.hits[0].char_offset, first.hits[0].char_offset);
    assert_eq!(third.hits[0].chapter_index, first.hits[0].chapter_index);
}
