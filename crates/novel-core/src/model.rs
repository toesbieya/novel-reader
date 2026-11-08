use serde::Serialize;

/// 前端能看到的章节信息。故意不含字节偏移：
/// 偏移是引擎内部实现细节，暴露出去只会让前端误用。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterMeta {
    pub index: usize,
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub chapter_index: usize,
    pub chapter_title: String,
    /// 章内字符偏移（不是字节），用于前端定位高亮
    pub char_offset: usize,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchOutcome {
    pub hits: Vec<Hit>,
    /// 命中总数，可能远大于 hits.len()（受 limit 截断）
    pub total: usize,
    pub elapsed_us: u128,
}

impl SearchOutcome {
    pub fn empty(elapsed_us: u128) -> Self {
        Self { hits: Vec::new(), total: 0, elapsed_us }
    }
}
