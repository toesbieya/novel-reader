//! 书架数据层里与"文件被外部改动"相关的行为。
//! 对应的用户可见结果是一条 Toast：「《xx》的文件变了，目录已重建」。

mod common;

use common::TempDir;
use novel_core::store::Library;

const CHAPTERS_A: &str = "第一章 甲\n正文。\n第二章 乙\n正文。\n";
const CHAPTERS_B: &str = "第一章 甲\n正文。\n第二章 乙\n正文。\n第三章 丙\n正文。\n";

#[test]
fn reopening_a_changed_file_reports_one_notice() {
    let dir = TempDir::new("reindex");
    let txt = dir.path().join("书.txt");
    let db = dir.path().join("library.db");
    std::fs::write(&txt, CHAPTERS_A).unwrap();

    let id = {
        let mut lib = Library::open(&db).unwrap();
        lib.import_book(&txt, None).unwrap().id
    };

    // 模拟"应用关闭期间文件被外部改掉"：重开库后第一次读章节才重建索引
    std::fs::write(&txt, CHAPTERS_B).unwrap();

    let mut lib = Library::open(&db).unwrap();
    lib.read_chapter(id, 0).unwrap();
    assert_eq!(lib.chapters(id).unwrap().len(), 3, "目录来自重建后的索引，不是旧表");

    let notices = lib.take_reindex_notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].book_id, id);
    assert_eq!(notices[0].title, "书");

    // 已是最新，再读不会重复通知
    lib.read_chapter(id, 0).unwrap();
    assert!(lib.take_reindex_notices().is_empty());
}

/// 文件被外部改动后重建索引，不能把 `char_len` 清零：
/// 清零会让书架进度退回按章节估算，并触发一次全本重解码。
#[test]
fn reindex_keeps_chapter_char_lengths() {
    let dir = TempDir::new("reindex-chars");
    let txt = dir.path().join("书.txt");
    let db = dir.path().join("library.db");
    std::fs::write(&txt, CHAPTERS_A).unwrap();

    let id = {
        let mut lib = Library::open(&db).unwrap();
        lib.import_book(&txt, None).unwrap().id
    };

    let count_chars = |lib: &Library| -> (i64, i64) {
        lib.conn()
            .query_row(
                "SELECT COALESCE(SUM(char_len), 0), COUNT(*) FROM chapter WHERE book_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    };

    let before = {
        let lib = Library::open(&db).unwrap();
        count_chars(&lib)
    };
    assert!(before.0 > 0, "导入时就应该算好字数");

    // 章节数不变、正文长度变了 → 下次读章节触发重建索引
    std::fs::write(&txt, "第一章 甲\n正文加长了一些。\n第二章 乙\n正文。\n").unwrap();

    let mut lib = Library::open(&db).unwrap();
    lib.read_chapter(id, 0).unwrap();

    let after = count_chars(&lib);
    assert_eq!(after.1, before.1, "章节数没变");
    assert_eq!(after.0, before.0, "重建后章节字数应原样保留");
}

/// 解析（prepare）可能跑几百毫秒，期间同一本书可能已被另一次导入写进去。
/// commit 里必须再查一次重，否则会撞 `file_path UNIQUE`。
#[test]
fn committing_the_same_file_twice_is_idempotent() {
    let dir = TempDir::new("prepared-import");
    let txt = dir.path().join("书.txt");
    let db = dir.path().join("library.db");
    std::fs::write(&txt, CHAPTERS_A).unwrap();

    let mut lib = Library::open(&db).unwrap();
    let first = Library::prepare_import(&txt).unwrap();
    let second = Library::prepare_import(&txt).unwrap();

    let a = lib.commit_import(first, None).unwrap();
    let b = lib.commit_import(second, None).unwrap();

    assert!(!a.already_exists);
    assert!(b.already_exists, "第二次提交应识别为已存在");
    assert_eq!(a.id, b.id);
    assert_eq!(lib.list_books().unwrap().len(), 1, "库里只能有一条记录");
}

#[test]
fn unchanged_file_reports_nothing() {
    let dir = TempDir::new("no-reindex");
    let txt = dir.path().join("书.txt");
    let db = dir.path().join("library.db");
    std::fs::write(&txt, CHAPTERS_A).unwrap();

    let id = {
        let mut lib = Library::open(&db).unwrap();
        lib.import_book(&txt, None).unwrap().id
    };

    let mut lib = Library::open(&db).unwrap();
    lib.read_chapter(id, 0).unwrap();
    assert!(lib.take_reindex_notices().is_empty(), "mtime/size 没变就不该提示");
}

#[test]
fn settings_persist_in_the_same_db() {
    let dir = TempDir::new("settings");
    let db = dir.path().join("library.db");

    {
        let lib = Library::open(&db).unwrap();
        assert_eq!(lib.get_setting("reader.prefs").unwrap(), None);
        lib.set_setting("reader.prefs", r#"{"theme":"sepia"}"#).unwrap();
    }

    let lib = Library::open(&db).unwrap();
    assert_eq!(
        lib.get_setting("reader.prefs").unwrap().as_deref(),
        Some(r#"{"theme":"sepia"}"#)
    );
}

#[test]
fn copy_db_creates_a_standalone_snapshot() {
    let dir = TempDir::new("copy-db");
    let src = dir.path().join("library.db");
    let dest = dir.path().join("moved.db");

    let lib = Library::open(&src).unwrap();
    lib.set_setting("window.state", r#"{"width":1280}"#).unwrap();
    lib.copy_db_to(&dest).unwrap();

    let moved = Library::open(&dest).unwrap();
    assert_eq!(
        moved.get_setting("window.state").unwrap().as_deref(),
        Some(r#"{"width":1280}"#)
    );
}
