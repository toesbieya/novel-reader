//! novel-core —— 本地小说阅读器引擎。
//!
//! 三条硬约束贯穿全部实现：
//! 1. **零副本**：只读原文件（mmap），不复制、不改写
//! 2. **偏移即索引**：章节存字节区间，打开书是查表而不是扫文件
//! 3. **零 UI 依赖**：这个 crate 不知道 Tauri 的存在，可以单独压测

pub mod encoding;
pub mod epub;
pub mod fonts;
pub mod model;
pub mod scan;
pub mod search;
pub mod store;
pub mod txt;
/// EPUB 搜索纯文本缓存（零副本的唯一例外），不对外暴露。
mod text_cache;

/// 指定 EPUB 搜索缓存根目录（Tauri 侧跟随用户数据目录）。启动时调用一次即可。
pub fn set_search_cache_dir(dir: std::path::PathBuf) {
    text_cache::set_root(dir);
}

use std::path::Path;

use thiserror::Error;

pub use epub::EpubBook;
pub use model::{ChapterMeta, Hit, SearchOutcome};
pub use txt::TxtBook;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Text(#[from] encoding::TextError),
    #[error(transparent)]
    Epub(#[from] epub::EpubError),
    #[error("{0}")]
    Unsupported(String),
}

pub enum Document {
    Txt(TxtBook),
    Epub(EpubBook),
}

impl Document {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "txt" => Ok(Document::Txt(TxtBook::open(path)?)),
            "epub" => Ok(Document::Epub(EpubBook::open(path)?)),
            other => Err(Error::Unsupported(format!(
                "不支持的格式 .{other}（只支持 txt / epub）"
            ))),
        }
    }

    pub fn chapters(&self) -> &[ChapterMeta] {
        match self {
            Document::Txt(b) => b.chapters(),
            Document::Epub(b) => b.chapters(),
        }
    }

    pub fn read_chapter(&self, idx: usize) -> Result<String, Error> {
        match self {
            Document::Txt(b) => b
                .read_chapter(idx)
                .ok_or_else(|| Error::Unsupported(format!("章节越界：{idx}"))),
            Document::Epub(b) => Ok(b.read_chapter(idx)?),
        }
    }

    pub fn search(&self, query: &str, limit: usize) -> SearchOutcome {
        self.search_paged(query, limit, 0)
    }

    /// 结果分页：跳过前 `offset` 处命中，`total` 仍是全书精确总数。
    pub fn search_paged(&self, query: &str, limit: usize, offset: usize) -> SearchOutcome {
        match self {
            Document::Txt(b) => b.search_paged(query, limit, offset),
            Document::Epub(b) => b.search_paged(query, limit, offset),
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Document::Txt(b) => b.title(),
            Document::Epub(b) => b.title(),
        }
    }

    pub fn encoding_name(&self) -> &'static str {
        match self {
            Document::Txt(b) => b.encoding_name(),
            Document::Epub(b) => b.encoding_name(),
        }
    }

    pub fn chapter_count(&self) -> usize {
        self.chapters().len()
    }

    pub fn file_size(&self) -> u64 {
        match self {
            Document::Txt(b) => b.file_size(),
            Document::Epub(b) => b.file_size(),
        }
    }

    pub fn format(&self) -> &'static str {
        match self {
            Document::Txt(_) => "txt",
            Document::Epub(_) => "epub",
        }
    }

    /// 章节在原文件里的字节区间；EPUB 是按 zip 条目读的，没有字节偏移概念。
    pub fn spans(&self) -> &[(usize, usize)] {
        match self {
            Document::Txt(b) => b.spans(),
            Document::Epub(_) => &[],
        }
    }
}
