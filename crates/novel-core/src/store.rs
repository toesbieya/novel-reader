//! 书架数据层：SQLite 持久化 + 打开文档的缓存。
//!
//! 三条纪律：
//! 1. **删除只删记录，绝不碰磁盘文件**（需求里的硬约束）
//! 2. 章节索引在导入时落库，之后出目录是查表，不重新扫文件
//! 3. 目录树深度由应用层强制 ≤5 级，SQLite 不做递归约束

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::UNIX_EPOCH;

use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use thiserror::Error;

use crate::model::{ChapterMeta, Hit};
use crate::Document;

/// 目录树最大层级（根目录算第 1 级）
pub const MAX_FOLDER_DEPTH: i64 = 5;

/// 文件失效重建的两段式中间产物。
/// `reindex_plan` 只在锁内查库 + stat，`Document::open`（全本切章）交给调用方放到锁外，
/// 最后 `reindex_apply` 短锁回写——否则一次外部改动会挡住所有翻页 / 存进度命令。
pub struct ReindexPlan {
    pub path: PathBuf,
    pub size: i64,
    pub mtime: i64,
    pub title: String,
}

/// 同时保持打开的文档上限。超了按最久未用淘汰，控制内存。
const OPEN_CACHE_LIMIT: usize = 3;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("数据库错误：{0}")]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Core(#[from] crate::Error),
    #[error("文件操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Rule(String),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub sort: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookSummary {
    pub id: i64,
    pub folder_id: Option<i64>,
    pub title: String,
    pub author: Option<String>,
    pub format: String,
    pub encoding: String,
    pub chapter_count: i64,
    /// 全书字符数（`chapter.char_len` 之和）。书架卡片用它展示"多少万字"。
    /// 老库没回填过时是 0，前端按"未知"处理，不显示。
    pub total_chars: i64,
    pub file_path: String,
    pub file_size: i64,
    pub cover_path: Option<String>,
    /// 0..1，按章节位置估算
    pub progress: f64,
    pub last_read_at: Option<i64>,
    pub missing: bool,
    /// 停在第十章（0 基）。没读过是 None——书架用它显示「第 312 章」，
    /// 注意它和 `progress` 不是一回事：progress 是按字数加权的百分比。
    pub last_chapter: Option<i64>,
    /// 入库时间（unix 秒）。「最近添加」排序用，也用于没有阅读记录时的活动时间。
    pub added_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterContent {
    pub index: usize,
    pub title: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub hits: Vec<Hit>,
    pub total: usize,
    pub elapsed_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub chapter_index: i64,
    pub char_offset: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    pub id: i64,
    pub title: String,
    pub chapter_count: i64,
    pub encoding: String,
    /// 该文件已在书架里，没有重复导入
    pub already_exists: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteOutcome {
    pub folders_removed: usize,
    pub books_removed: usize,
}

/// 文件被外部改动后重建索引的通知。核心层只记账，
/// 由 Tauri 层取走并推给前端——`novel-core` 不认识 UI。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReindexNotice {
    /// 重建过索引的书
    pub book_id: i64,
    /// 书名，前端提示文案直接用它
    pub title: String,
}

/// 导入前在锁外算好的东西：打开文档、切章、算全书字数。
///
/// 这几步占导入 99% 的时间（30MB 约 0.3s）。调用方先在锁外产出它，
/// 再只拿写库那一小段锁，导入期间翻页 / 存进度就不会被这把锁挡住。
pub struct PreparedImport {
    /// 规范化后的绝对路径（已去掉 `\\?\` 前缀），写库和去重都用它
    key: String,
    doc: Document,
    file_size: i64,
    file_mtime: i64,
    title: String,
    author: Option<String>,
    encoding: String,
    format: &'static str,
    chapters: Vec<ChapterMeta>,
    spans: Vec<(usize, usize)>,
    char_lens: Vec<i64>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS folder (
  id         INTEGER PRIMARY KEY,
  parent_id  INTEGER REFERENCES folder(id) ON DELETE CASCADE,
  name       TEXT    NOT NULL,
  sort       INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS book (
  id            INTEGER PRIMARY KEY,
  folder_id     INTEGER REFERENCES folder(id) ON DELETE SET NULL,
  title         TEXT    NOT NULL,
  author        TEXT,
  format        TEXT    NOT NULL,
  file_path     TEXT    NOT NULL UNIQUE,
  file_size     INTEGER NOT NULL,
  file_mtime    INTEGER NOT NULL,
  encoding      TEXT    NOT NULL,
  chapter_count INTEGER NOT NULL DEFAULT 0,
  cover_path    TEXT,
  missing       INTEGER NOT NULL DEFAULT 0,
  added_at      INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS chapter (
  book_id INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  idx     INTEGER NOT NULL,
  title   TEXT    NOT NULL,
  start   INTEGER NOT NULL,
  end     INTEGER NOT NULL,
  -- 本章字符数，进度按字数加权用。老库靠 ensure_columns 补列后为 0。
  char_len INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (book_id, idx)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS progress (
  book_id     INTEGER PRIMARY KEY REFERENCES book(id) ON DELETE CASCADE,
  chapter_idx INTEGER NOT NULL,
  char_offset INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS setting (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_book_folder ON book(folder_id);
CREATE INDEX IF NOT EXISTS idx_folder_parent ON folder(parent_id);
"#;

static AUTHOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:作\s*者|著\s*者)\s*[:：]\s*([^\r\n]{1,20})").expect("作者正则写错了")
});

pub struct Library {
    conn: Connection,
    /// 打开过的文档缓存：book_id → Document。读章节不用每次重新切分。
    open: HashMap<i64, Document>,
    /// 粗略的 LRU：越靠后越新
    lru: Vec<i64>,
    /// 待推给前端的"文件变了"通知，由 `take_reindex_notices` 取走
    notices: Vec<ReindexNotice>,
}

impl Library {
    pub fn open(db_path: &Path) -> Result<Self, StoreError> {
        if let Some(dir) = db_path.parent() {
            if !dir.as_os_str().is_empty() {
                fs::create_dir_all(dir)?;
            }
        }
        let conn = Connection::open(db_path)?;
        // journal_mode 会返回一行结果，不能用 execute_batch
        let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
        conn.execute_batch(SCHEMA)?;
        ensure_columns(&conn)?;

        Ok(Library { conn, open: HashMap::new(), lru: Vec::new(), notices: Vec::new() })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    // ------------------------------------------------------------ 目录树

    pub fn create_folder(&mut self, parent_id: Option<i64>, name: &str) -> Result<i64, StoreError> {
        let name = validate_folder_name(name)?;

        let depth = match parent_id {
            Some(pid) => self.folder_depth(pid)?,
            None => 0,
        };
        if depth + 1 > MAX_FOLDER_DEPTH {
            return Err(StoreError::Rule(format!("目录最多 {MAX_FOLDER_DEPTH} 级")));
        }

        // 同级不允许重名，否则用户分不清
        let dup: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM folder WHERE name = ?1 AND parent_id IS ?2",
                params![name, parent_id],
                |r| r.get(0),
            )
            .optional()?;
        if dup.is_some() {
            return Err(StoreError::Rule(format!("同级已有同名目录：{name}")));
        }

        let sort: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(sort), -1) + 1 FROM folder WHERE parent_id IS ?1",
                params![parent_id],
                |r| r.get(0),
            )
            .unwrap_or(0);

        self.conn.execute(
            "INSERT INTO folder (parent_id, name, sort, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![parent_id, name, sort, now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 按名字找同级目录，不存在就建；返回 `(id, 是否新建)`。
    ///
    /// 「导入文件夹」用：磁盘上的每一级子目录对应一个分组，重复导入同一棵树时
    /// 同名目录直接复用（合并），不会堆出一串「玄幻 (2)」。
    /// 不同父级下的同名目录互不影响——合并只发生在同一级。
    pub fn ensure_folder(
        &mut self,
        parent_id: Option<i64>,
        name: &str,
    ) -> Result<(i64, bool), StoreError> {
        let name = validate_folder_name(name)?;
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM folder WHERE name = ?1 AND parent_id IS ?2",
                params![name, parent_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok((id, false));
        }
        let id = self.create_folder(parent_id, name)?;
        Ok((id, true))
    }

    pub fn rename_folder(&mut self, id: i64, name: &str) -> Result<(), StoreError> {
        let name = validate_folder_name(name)?;
        let n = self.conn.execute("UPDATE folder SET name = ?1 WHERE id = ?2", params![name, id])?;
        if n == 0 {
            return Err(StoreError::Rule(format!("目录不存在：{id}")));
        }
        Ok(())
    }

    /// 移动目录。会校验目标深度，防止把一棵子树挪到第 5 级下面。
    pub fn move_folder(&mut self, id: i64, new_parent: Option<i64>) -> Result<(), StoreError> {
        if Some(id) == new_parent {
            return Err(StoreError::Rule("不能把目录挪到自己下面".into()));
        }
        if let Some(pid) = new_parent {
            if self.is_descendant(pid, id)? {
                return Err(StoreError::Rule("不能把目录挪到自己的子目录下".into()));
            }
        }

        let subtree_height = self.subtree_height(id)?;
        let target_depth = match new_parent {
            Some(pid) => self.folder_depth(pid)?,
            None => 0,
        };
        if target_depth + subtree_height > MAX_FOLDER_DEPTH {
            return Err(StoreError::Rule(format!("移动后超过 {MAX_FOLDER_DEPTH} 级上限")));
        }

        self.conn
            .execute("UPDATE folder SET parent_id = ?1 WHERE id = ?2", params![new_parent, id])?;
        Ok(())
    }

    /// 同级排序：把 `id` 挪到 `target_id` 之前（after=false）或之后（after=true）。
    /// 只重写 `sort`，不改 `parent_id`——跨层级拖拽请走 `move_folder`。
    /// 落库的 sort 会重新压成 0..n-1，避免反复拖拽后数值发散。
    pub fn reorder_folder(&mut self, id: i64, target_id: i64, after: bool) -> Result<(), StoreError> {
        if id == target_id {
            return Ok(());
        }
        let parent = self.folder_parent(id)?;
        if parent != self.folder_parent(target_id)? {
            return Err(StoreError::Rule("只能在同一级目录内排序".into()));
        }

        let mut ids: Vec<i64> = {
            let mut stmt =
                self.conn.prepare("SELECT id FROM folder WHERE parent_id IS ?1 ORDER BY sort, id")?;
            let rows = stmt.query_map(params![parent], |r| r.get(0))?;
            rows.collect::<Result<Vec<i64>, _>>()?
        };
        ids.retain(|x| *x != id);
        let pos = ids
            .iter()
            .position(|x| *x == target_id)
            .ok_or_else(|| StoreError::Rule("目标目录不在这棵子树里".into()))?;
        ids.insert(if after { pos + 1 } else { pos }, id);

        let tx = self.conn.transaction()?;
        for (i, fid) in ids.iter().enumerate() {
            tx.execute("UPDATE folder SET sort = ?1 WHERE id = ?2", params![i as i64, fid])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_folders(&self) -> Result<Vec<Folder>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_id, name, sort FROM folder ORDER BY COALESCE(parent_id, -1), sort, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Folder { id: r.get(0)?, parent_id: r.get(1)?, name: r.get(2)?, sort: r.get(3)? })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// 删除目录及其全部子目录、以及其中的书籍记录。
    /// **不碰磁盘上的任何文件。**
    pub fn delete_folder(&mut self, id: i64) -> Result<DeleteOutcome, StoreError> {
        let ids = self.subtree_ids(id)?;
        if ids.is_empty() {
            return Err(StoreError::Rule(format!("目录不存在：{id}")));
        }

        let tx = self.conn.transaction()?;
        let mut books = 0usize;
        for fid in &ids {
            let n: usize = tx.query_row(
                "SELECT COUNT(*) FROM book WHERE folder_id = ?1",
                params![fid],
                |r| r.get(0),
            )?;
            books += n;
            tx.execute("DELETE FROM book WHERE folder_id = ?1", params![fid])?;
        }
        // folder 之间是 ON DELETE CASCADE，删根即删整棵子树
        tx.execute("DELETE FROM folder WHERE id = ?1", params![id])?;
        tx.commit()?;

        // 缓存里可能还有被删书籍的文档
        self.evict_not_in_db()?;

        Ok(DeleteOutcome { folders_removed: ids.len(), books_removed: books })
    }

    // ------------------------------------------------------------ 书籍

    /// 便宜的去重检查：只查一条索引，避免为已在书架里的书重切一遍。
    /// 调用方拿到 `Some` 就可以直接返回，不用进 `prepare_import` 的重活。
    pub fn already_imported(&self, path: &Path) -> Result<Option<ImportOutcome>, StoreError> {
        let (_, key) = canonical_key(path)?;
        Ok(self.find_book_by_path(&key)?.map(|(id, title, chapter_count, encoding)| {
            ImportOutcome { id, title, chapter_count, encoding, already_exists: true }
        }))
    }

    /// 重活：打开、切章、算全书字数。**不碰数据库**，调用方应当在锁外调用。
    pub fn prepare_import(path: &Path) -> Result<PreparedImport, StoreError> {
        let (abs, key) = canonical_key(path)?;
        let doc = Document::open(&abs)?;
        let meta = fs::metadata(&abs)?;
        let size = meta.len() as i64;
        let mtime = mtime_secs(&meta);

        let raw_title = doc.title().to_string();
        let (title, author) = match &doc {
            Document::Txt(_) => {
                // 网文文件名常见「作者-书名」，拆开比整串塞进标题好看得多
                let (t, prefix_author) = split_stem(&raw_title);
                (t, sniff_author(&abs).or(prefix_author))
            }
            Document::Epub(b) => (raw_title, b.author().map(|s| s.to_string())),
        };
        let encoding = doc.encoding_name().to_string();
        let format = doc.format();
        let chapters = doc.chapters().to_vec();
        let spans = doc.spans().to_vec();

        // 每章字符数：进度按字数加权的前提。
        // 这一步要把整本书解一遍，是导入变慢的唯一原因——只算一次，且在锁外算。
        let char_lens: Vec<i64> = (0..chapters.len())
            .map(|i| doc.read_chapter(i).map(|t| t.chars().count() as i64).unwrap_or(0))
            .collect();

        Ok(PreparedImport {
            key,
            doc,
            file_size: size,
            file_mtime: mtime,
            title,
            author,
            encoding,
            format,
            chapters,
            spans,
            char_lens,
        })
    }

    /// 轻活：写库 + 进缓存。拿锁的时间只够一次事务。
    pub fn commit_import(
        &mut self,
        prepared: PreparedImport,
        folder_id: Option<i64>,
    ) -> Result<ImportOutcome, StoreError> {
        // 解析要几百毫秒，期间可能已经有另一次导入把同一本书写进去了
        if let Some((id, title, chapter_count, encoding)) = self.find_book_by_path(&prepared.key)? {
            return Ok(ImportOutcome {
                id,
                title,
                chapter_count,
                encoding,
                already_exists: true,
            });
        }

        let PreparedImport {
            key,
            doc,
            file_size,
            file_mtime,
            title,
            author,
            encoding,
            format,
            chapters,
            spans,
            char_lens,
        } = prepared;

        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO book (folder_id, title, author, format, file_path, file_size, file_mtime,
                               encoding, chapter_count, cover_path, missing, added_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, 0, ?10)",
            params![
                folder_id,
                title,
                author,
                format,
                key,
                file_size,
                file_mtime,
                encoding,
                chapters.len() as i64,
                now()
            ],
        )?;
        let id = tx.last_insert_rowid();

        {
            let mut stmt = tx.prepare(
                "INSERT INTO chapter (book_id, idx, title, start, end, char_len)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for (i, ch) in chapters.iter().enumerate() {
                let (start, end) = spans.get(i).copied().unwrap_or((0, 0));
                let len = char_lens.get(i).copied().unwrap_or(0);
                stmt.execute(params![id, ch.index as i64, ch.title, start as i64, end as i64, len])?;
            }
        }
        tx.commit()?;

        self.remember(id, doc);

        Ok(ImportOutcome {
            id,
            title,
            chapter_count: chapters.len() as i64,
            encoding,
            already_exists: false,
        })
    }

    /// 一把梭的便捷入口（CLI / 测试 / 单线程调用）。
    /// Tauri 的导入走 `prepare_import` + `commit_import`，中间那段重活不持锁。
    pub fn import_book(
        &mut self,
        path: &Path,
        folder_id: Option<i64>,
    ) -> Result<ImportOutcome, StoreError> {
        if let Some(out) = self.already_imported(path)? {
            return Ok(out);
        }
        let prepared = Self::prepare_import(path)?;
        self.commit_import(prepared, folder_id)
    }

    pub fn list_books(&self) -> Result<Vec<BookSummary>, StoreError> {
        // 进度按**字数**加权而不是章节序号：网文一章 1500 字和 6000 字混在一起，
        // 按序号算的话读到第 500/1000 章会被显示成 50%，实际可能才 35%。
        // 三个子查询都是按 (book_id, idx) 主键扫，54k 章的库实测只有个位数毫秒。
        let mut stmt = self.conn.prepare(
            "SELECT b.id, b.folder_id, b.title, b.author, b.format, b.encoding, b.chapter_count,
                    b.file_path, b.file_size, b.cover_path, b.missing,
                    p.chapter_idx, p.char_offset, p.updated_at,
                    (SELECT COALESCE(SUM(char_len), 0) FROM chapter c WHERE c.book_id = b.id),
                    (SELECT COALESCE(SUM(char_len), 0) FROM chapter c
                       WHERE c.book_id = b.id AND c.idx < COALESCE(p.chapter_idx, 0)),
                    (SELECT COALESCE(char_len, 0) FROM chapter c
                       WHERE c.book_id = b.id AND c.idx = COALESCE(p.chapter_idx, 0)),
                    b.added_at
             FROM book b
             LEFT JOIN progress p ON p.book_id = b.id
             ORDER BY COALESCE(p.updated_at, b.added_at) DESC",
        )?;

        let rows = stmt.query_map([], |r| {
            let chapter_count: i64 = r.get(6)?;
            let last_idx: Option<i64> = r.get(11)?;
            let char_offset: i64 = r.get::<_, Option<i64>>(12)?.unwrap_or(0);
            let total_chars: i64 = r.get(14)?;
            let done_chars: i64 = r.get(15)?;
            let cur_chars: i64 = r.get(16)?;

            let progress = match last_idx {
                // 有字数信息 → 加权；老库没回填过（total_chars = 0）就回退到按章节估算
                Some(_) if total_chars > 0 && cur_chars > 0 => {
                    let within = (char_offset.max(0).min(cur_chars) as f64) / cur_chars as f64;
                    ((done_chars as f64 + within * cur_chars as f64) / total_chars as f64)
                        .clamp(0.0, 1.0)
                }
                Some(i) if chapter_count > 0 => (i as f64 / chapter_count as f64).clamp(0.0, 1.0),
                _ => 0.0,
            };
            Ok(BookSummary {
                id: r.get(0)?,
                folder_id: r.get(1)?,
                title: r.get(2)?,
                author: r.get(3)?,
                format: r.get(4)?,
                encoding: r.get(5)?,
                chapter_count,
                total_chars,
                file_path: r.get(7)?,
                file_size: r.get(8)?,
                cover_path: r.get(9)?,
                missing: r.get::<_, i64>(10)? != 0,
                progress,
                last_read_at: r.get(13)?,
                last_chapter: last_idx,
                added_at: r.get(17)?,
            })
        })?;

        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// 还没算过字数的书（book_id, file_path）。
    pub fn char_lens_pending(&self) -> Result<Vec<(i64, String)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT b.id, b.file_path FROM book b
             WHERE b.chapter_count > 0
               AND (SELECT COALESCE(SUM(char_len), 0) FROM chapter c WHERE c.book_id = b.id) = 0",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// 写入某本书的章节字数（按 idx 对齐，缺的章节不动）。
    pub fn set_char_lens(&mut self, book_id: i64, lens: &[i64]) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt =
                tx.prepare("UPDATE chapter SET char_len = ?1 WHERE book_id = ?2 AND idx = ?3")?;
            for (i, len) in lens.iter().enumerate() {
                stmt.execute(params![len, book_id, i as i64])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// 给升级前入库的老书补 `char_len`。返回补了多少本。
    ///
    /// 只处理"这本书所有章节 char_len 都是 0"的情况，已经算过的直接跳过，
    /// 所以可以每次启动都调，代价只是一条聚合查询。
    pub fn backfill_char_lens(&mut self) -> Result<usize, StoreError> {
        let pending = self.char_lens_pending()?;
        let mut done = 0usize;

        for (id, path) in pending {
            let p = io_path(&path);
            if !p.is_file() {
                continue; // 文件没了就先跳过，等它回来再说
            }
            let Ok(doc) = Document::open(&p) else {
                continue;
            };

            let lens: Vec<i64> = (0..doc.chapter_count())
                .map(|i| doc.read_chapter(i).map(|t| t.chars().count() as i64).unwrap_or(0))
                .collect();

            self.set_char_lens(id, &lens)?;
            done += 1;
        }

        Ok(done)
    }

    pub fn set_book_folder(&mut self, book_id: i64, folder_id: Option<i64>) -> Result<(), StoreError> {
        self.conn.execute("UPDATE book SET folder_id = ?1 WHERE id = ?2", params![folder_id, book_id])?;
        Ok(())
    }

    /// 从书架移除一本书。**只删记录，原文件原封不动。**
    pub fn delete_book(&mut self, book_id: i64) -> Result<(), StoreError> {
        let n = self.conn.execute("DELETE FROM book WHERE id = ?1", params![book_id])?;
        if n == 0 {
            return Err(StoreError::Rule(format!("书籍不存在：{book_id}")));
        }
        self.open.remove(&book_id);
        self.lru.retain(|id| *id != book_id);
        Ok(())
    }

    pub fn chapters(&self, book_id: i64) -> Result<Vec<ChapterMeta>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT idx, title FROM chapter WHERE book_id = ?1 ORDER BY idx")?;
        let rows = stmt.query_map(params![book_id], |r| {
            Ok(ChapterMeta { index: r.get::<_, i64>(0)? as usize, title: r.get(1)? })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn read_chapter(&mut self, book_id: i64, index: usize) -> Result<ChapterContent, StoreError> {
        let title: String = self
            .conn
            .query_row(
                "SELECT title FROM chapter WHERE book_id = ?1 AND idx = ?2",
                params![book_id, index as i64],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::Rule(format!("章节不存在：{book_id}#{index}")))?;

        let doc = self.document(book_id)?;
        let text = doc.read_chapter(index)?;
        Ok(ChapterContent { index, title, text })
    }

    pub fn search(
        &mut self,
        book_id: i64,
        query: &str,
        limit: usize,
    ) -> Result<SearchResult, StoreError> {
        self.search_paged(book_id, query, limit, 0)
    }

    /// 搜索结果分页：`offset` 之前的命中只计数，`total` 仍是全书精确总数。
    pub fn search_paged(
        &mut self,
        book_id: i64,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<SearchResult, StoreError> {
        let doc = self.document(book_id)?;
        let out = doc.search_paged(query, limit, offset);
        Ok(SearchResult {
            hits: out.hits,
            total: out.total,
            elapsed_ms: out.elapsed_us as f64 / 1000.0,
        })
    }

    // ------------------------------------------------------------ 进度

    pub fn save_progress(
        &mut self,
        book_id: i64,
        chapter_index: i64,
        char_offset: i64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO progress (book_id, chapter_idx, char_offset, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(book_id) DO UPDATE SET
               chapter_idx = excluded.chapter_idx,
               char_offset = excluded.char_offset,
               updated_at  = excluded.updated_at",
            params![book_id, chapter_index, char_offset, now()],
        )?;
        Ok(())
    }

    pub fn progress(&self, book_id: i64) -> Result<Option<Progress>, StoreError> {
        let p = self
            .conn
            .query_row(
                "SELECT chapter_idx, char_offset, updated_at FROM progress WHERE book_id = ?1",
                params![book_id],
                |r| {
                    Ok(Progress {
                        chapter_index: r.get(0)?,
                        char_offset: r.get(1)?,
                        updated_at: r.get(2)?,
                    })
                },
            )
            .optional()?;
        Ok(p)
    }

    // ------------------------------------------------------------ 文件状态

    /// 逐个 stat 书架里的书，只更新 missing 标记。启动时调一次。
    pub fn refresh_missing(&mut self) -> Result<usize, StoreError> {
        let items: Vec<(i64, String)> = {
            let mut stmt = self.conn.prepare("SELECT id, file_path FROM book")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        let mut changed = 0usize;
        for (id, path) in items {
            let exists = io_path(&path).is_file();
            let n = self.conn.execute(
                "UPDATE book SET missing = ?1 WHERE id = ?2 AND missing != ?1",
                params![if exists { 0 } else { 1 }, id],
            )?;
            changed += n;
            if !exists {
                self.open.remove(&id);
            }
        }
        Ok(changed)
    }

    /// 第一段：只做查库 + stat，返回需要重建的计划；**不打开文档**。
    /// Tauri 层拿到 `ReindexPlan` 后在锁外 `Document::open`（30MB 切章几十毫秒），
    /// 再调 `reindex_apply` 短锁回写，避免重建期间挡住翻页 / 存进度。
    pub fn reindex_plan(&mut self, book_id: i64) -> Result<Option<ReindexPlan>, StoreError> {
        let (path, size, mtime, title): (String, i64, i64, String) = self
            .conn
            .query_row(
                "SELECT file_path, file_size, file_mtime, title FROM book WHERE id = ?1",
                params![book_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::Rule(format!("书籍不存在：{book_id}")))?;

        let meta = match fs::metadata(io_path(&path)) {
            Ok(m) => m,
            Err(_) => {
                self.conn.execute("UPDATE book SET missing = 1 WHERE id = ?1", params![book_id])?;
                self.open.remove(&book_id);
                return Ok(None);
            }
        };

        let new_size = meta.len() as i64;
        let new_mtime = mtime_secs(&meta);
        if new_size == size && new_mtime == mtime {
            return Ok(None);
        }

        Ok(Some(ReindexPlan { path: io_path(&path), size: new_size, mtime: new_mtime, title }))
    }

    /// 命令行 / 非 Tauri 路径用：一条龙重建（CLI 单线程，没有 UI 等锁的问题）。
    pub fn reindex_if_stale(&mut self, book_id: i64) -> Result<bool, StoreError> {
        let Some(plan) = self.reindex_plan(book_id)? else {
            return Ok(false);
        };
        let doc = Document::open(&plan.path)?;
        self.reindex_apply(book_id, plan, doc)
    }

    /// 第二段：把锁外打开好的文档写回章节表 + LRU + 重建通知。
    pub fn reindex_apply(
        &mut self,
        book_id: i64,
        plan: ReindexPlan,
        doc: Document,
    ) -> Result<bool, StoreError> {
        let (new_size, new_mtime, title) = (plan.size, plan.mtime, plan.title);
        let chapters = doc.chapters().to_vec();
        let spans = doc.spans().to_vec();

        // 旧表里的章节字数。重建索引通常只是文件被 touch、或者改了几个错别字，
        // 章节数不变时按 idx 沿用即可；顺手清零会让书架进度退回按章节估算，
        // 并在下次进书架时触发一次全本重解码（30MB 约 0.3s）。
        let old_chars: HashMap<i64, i64> = {
            let mut stmt = self.conn.prepare("SELECT idx, char_len FROM chapter WHERE book_id = ?1")?;
            let rows = stmt.query_map(params![book_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<Result<HashMap<i64, i64>, _>>()?
        };
        // 章节数变了说明切分结果不可比，宁可留 0 交给后台回填重算
        let keep_chars = old_chars.len() == chapters.len();

        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM chapter WHERE book_id = ?1", params![book_id])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO chapter (book_id, idx, title, start, end, char_len)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for (i, ch) in chapters.iter().enumerate() {
                let (start, end) = spans.get(i).copied().unwrap_or((0, 0));
                let idx = ch.index as i64;
                let len = if keep_chars { old_chars.get(&idx).copied().unwrap_or(0) } else { 0 };
                stmt.execute(params![book_id, idx, ch.title, start as i64, end as i64, len])?;
            }
        }
        tx.execute(
            "UPDATE book SET file_size = ?1, file_mtime = ?2, chapter_count = ?3,
                             encoding = ?4, missing = 0
             WHERE id = ?5",
            params![new_size, new_mtime, chapters.len() as i64, doc.encoding_name(), book_id],
        )?;
        tx.commit()?;

        self.remember(book_id, doc);
        self.notices.push(ReindexNotice { book_id, title });
        Ok(true)
    }

    /// 取走攒下的重建通知（取完即清空）。
    /// Tauri 层在读章节 / 搜索之后调用，把事件推给前端。
    pub fn take_reindex_notices(&mut self) -> Vec<ReindexNotice> {
        std::mem::take(&mut self.notices)
    }

    // ------------------------------------------------------------ 设置

    /// 读一条应用设置（阅读偏好、书架排序、窗口状态等）。
    pub fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT value FROM setting WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?)
    }

    /// 写一条应用设置。设置和书库在同一个 SQLite 文件里，搬迁时只需要一个 db。
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO setting (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// 用 `VACUUM INTO` 复制出一份一致的数据库快照（包含 WAL 里的最新事务）。
    pub fn copy_db_to(&self, dest: &Path) -> Result<(), StoreError> {
        if dest.exists() {
            return Err(StoreError::Rule(format!("目标文件已存在：{}", dest.display())));
        }
        self.conn.execute("VACUUM INTO ?1", params![dest.to_string_lossy()])?;
        Ok(())
    }

    // ------------------------------------------------------------ 内部

    fn document(&mut self, book_id: i64) -> Result<&Document, StoreError> {
        // 命中 LRU 也必须 stat：文件可能在应用运行期间被外部改写（网盘同步、编辑器另存），
        // 否则一直读旧 mmap，旧的章节字节偏移甚至可能越界。
        self.reindex_if_stale(book_id)?;
        if self.open.contains_key(&book_id) {
            self.lru.retain(|id| *id != book_id);
            self.lru.push(book_id);
        } else {
            let path: String = self
                .conn
                .query_row(
                    "SELECT file_path FROM book WHERE id = ?1",
                    params![book_id],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or_else(|| StoreError::Rule(format!("书籍不存在：{book_id}")))?;
            let doc = Document::open(&io_path(&path))?;
            self.remember(book_id, doc);
        }

        Ok(self.open.get(&book_id).expect("上面两个分支都保证了它有值"))
    }

    fn remember(&mut self, id: i64, doc: Document) {
        self.open.insert(id, doc);
        self.lru.retain(|x| *x != id);
        self.lru.push(id);
        while self.lru.len() > OPEN_CACHE_LIMIT {
            let victim = self.lru.remove(0);
            self.open.remove(&victim);
        }
    }

    fn evict_not_in_db(&mut self) -> Result<(), StoreError> {
        let live: Vec<i64> = {
            let mut stmt = self.conn.prepare("SELECT id FROM book")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        self.open.retain(|id, _| live.contains(id));
        self.lru.retain(|id| live.contains(id));
        Ok(())
    }

    fn find_book_by_path(
        &self,
        path: &str,
    ) -> Result<Option<(i64, String, i64, String)>, StoreError> {
        let r = self
            .conn
            .query_row(
                "SELECT id, title, chapter_count, encoding FROM book WHERE file_path = ?1",
                params![path],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        Ok(r)
    }

    fn folder_depth(&self, id: i64) -> Result<i64, StoreError> {
        let mut depth = 1i64;
        let mut cur = id;
        loop {
            let parent: Option<i64> = self
                .conn
                .query_row("SELECT parent_id FROM folder WHERE id = ?1", params![cur], |r| r.get(0))
                .optional()?
                .ok_or_else(|| StoreError::Rule(format!("目录不存在：{id}")))?;
            match parent {
                Some(p) => {
                    depth += 1;
                    cur = p;
                    if depth > MAX_FOLDER_DEPTH + 2 {
                        return Err(StoreError::Rule("目录树出现环".into()));
                    }
                }
                None => break,
            }
        }
        Ok(depth)
    }

    /// 目录的父节点；目录不存在时报错，而不是静默返回 None（None 表示"在根上"）。
    fn folder_parent(&self, id: i64) -> Result<Option<i64>, StoreError> {
        self.conn
            .query_row("SELECT parent_id FROM folder WHERE id = ?1", params![id], |r| r.get(0))
            .optional()?
            .ok_or_else(|| StoreError::Rule(format!("目录不存在：{id}")))
    }

    fn subtree_ids(&self, root: i64) -> Result<Vec<i64>, StoreError> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE sub(id) AS (
               SELECT id FROM folder WHERE id = ?1
               UNION ALL
               SELECT f.id FROM folder f JOIN sub s ON f.parent_id = s.id
             )
             SELECT id FROM sub",
        )?;
        let rows = stmt.query_map(params![root], |r| r.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// 子树从自己到最深叶子有几层
    fn subtree_height(&self, root: i64) -> Result<i64, StoreError> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE sub(id, lvl) AS (
               SELECT id, 1 FROM folder WHERE id = ?1
               UNION ALL
               SELECT f.id, s.lvl + 1 FROM folder f JOIN sub s ON f.parent_id = s.id
             )
             SELECT COALESCE(MAX(lvl), 0) FROM sub",
        )?;
        let h: i64 = stmt.query_row(params![root], |r| r.get(0))?;
        Ok(h)
    }

    fn is_descendant(&self, candidate: i64, ancestor: i64) -> Result<bool, StoreError> {
        let ids = self.subtree_ids(ancestor)?;
        Ok(ids.contains(&candidate))
    }
}

// ---------------------------------------------------------------- 小工具

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn mtime_secs(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 库里存的是剥掉 `\\?\` 的展示 / 去重路径，但 Windows 上超过 260 字符的路径
/// 和 UNC 路径**必须**带 verbatim 前缀才能被 Win32 API 打开。所有从 db 取路径做 IO
/// 的地方都走这里，否则长路径书会被误判成 `missing`、或导入后打不开。
fn io_path(key: &str) -> PathBuf {
    if key.starts_with(r"\\?\") {
        return PathBuf::from(key);
    }
    if key.starts_with("UNC\\") || key.chars().nth(1) == Some(':') {
        return PathBuf::from(format!(r"\\?\{key}"));
    }
    PathBuf::from(key)
}

#[cfg(test)]
mod tests {
    use super::io_path;

    #[test]
    fn io_path_restores_verbatim_prefix_for_long_paths_and_unc() {
        assert_eq!(io_path(r"G:\books\a.txt").to_string_lossy(), r"\\?\G:\books\a.txt");
        assert_eq!(
            io_path(r"UNC\server\share\a.txt").to_string_lossy(),
            r"\\?\UNC\server\share\a.txt"
        );
        // 已经是 verbatim 的不要重复加前缀
        assert_eq!(io_path(r"\\?\G:\books\a.txt").to_string_lossy(), r"\\?\G:\books\a.txt");
    }
}

/// 规范化路径，并去掉 Windows `canonicalize` 产生的 `\\?\` 前缀
/// （看着难受，而且和用户手输的路径不好比对）。
fn canonical_key(path: &Path) -> Result<(PathBuf, String), StoreError> {
    let abs = fs::canonicalize(path).map_err(|e| {
        StoreError::Io(std::io::Error::new(e.kind(), format!("{}: {e}", path.display())))
    })?;
    let key = abs
        .to_string_lossy()
        .strip_prefix(r"\\?\")
        .map(|s| s.to_string())
        .unwrap_or_else(|| abs.to_string_lossy().to_string());
    Ok((abs, key))
}

/// 老库补列。`CREATE TABLE IF NOT EXISTS` 不会给已存在的表加字段，
/// schema 一升级就必须显式迁移，否则老用户打开就报 "no such column"。
fn ensure_columns(conn: &Connection) -> Result<(), StoreError> {
    let has: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('chapter') WHERE name = 'char_len'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if has.is_none() {
        conn.execute("ALTER TABLE chapter ADD COLUMN char_len INTEGER NOT NULL DEFAULT 0", [])?;
    }
    Ok(())
}

fn validate_folder_name(name: &str) -> Result<&str, StoreError> {
    let t = name.trim();
    if t.is_empty() {
        return Err(StoreError::Rule("目录名不能为空".into()));
    }
    if t.chars().count() > 60 {
        return Err(StoreError::Rule("目录名太长".into()));
    }
    if t.contains('/') || t.contains('\\') {
        return Err(StoreError::Rule("目录名不能包含 / 或 \\".into()));
    }
    Ok(t)
}

/// 「zhttty-大宇宙时代」→ ("大宇宙时代", Some("zhttty"))。
/// 只在连字符左侧足够短（≤12 字）时才认定是作者名，避免把带连字符的书名拆坏。
fn split_stem(stem: &str) -> (String, Option<String>) {
    for sep in ['-', '－', '—'] {
        if let Some((left, right)) = stem.split_once(sep) {
            let (l, r) = (left.trim(), right.trim());
            if !l.is_empty() && !r.is_empty() && l.chars().count() <= 12 {
                return (r.to_string(), Some(l.to_string()));
            }
        }
    }
    (stem.trim().to_string(), None)
}

/// 从 TXT 头部找「作者：XXX」。找不到就返回 None，不猜。
fn sniff_author(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 4096];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    let text = crate::encoding::decode_loose(&buf);
    for line in text.lines().take(30) {
        if let Some(c) = AUTHOR_RE.captures(line) {
            let a = c.get(1)?.as_str().trim();
            if !a.is_empty() {
                return Some(a.to_string());
            }
        }
    }
    None
}
