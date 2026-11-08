//! EPUB 解析。
//!
//! 只做阅读器需要的那部分：container → opf → spine + toc → 逐章抽文本。
//! zip 随机访问，不整包解压；正文按章按需读取。

use std::cell::RefCell;
use std::collections::{hash_map::DefaultHasher, HashMap};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use thiserror::Error;
use zip::ZipArchive;

use crate::model::ChapterMeta;

/// 单个 zip 条目最多读这么多字节：损坏 EPUB / zip 炸弹不该把内存吃光。
const MAX_ENTRY_BYTES: u64 = 8 << 20;

#[derive(Debug, Error)]
pub enum EpubError {
    #[error("打不开 EPUB：{0}")]
    Open(String),
    #[error("EPUB 结构异常：{0}")]
    Structure(String),
    #[error("读取失败：{0}")]
    Io(String),
}

struct Item {
    href: String,
    media_type: String,
    properties: String,
}

struct Package {
    title: String,
    /// `dc:creator`，没有就 None（不猜）
    author: Option<String>,
    manifest: HashMap<String, Item>,
    spine: Vec<String>,
    toc_id: Option<String>,
}

pub struct EpubBook {
    pub(crate) chapters: Vec<ChapterMeta>,
    hrefs: Vec<String>,
    archive: RefCell<ZipArchive<BufReader<File>>>,
    title: String,
    /// opf 里的 `dc:creator`，没有就 None
    author: Option<String>,
    path: PathBuf,
    file_size: u64,
    /// EPUB 文件的修改时间（毫秒），纯文本缓存的失效依据之一
    mtime: u64,
    /// 搜索缓存文件位置；拿不到 `%LOCALAPPDATA%` 时为 None
    cache_path: Option<PathBuf>,
    /// 搜索专用的整本纯文本。首次搜索时解压并落盘，**不参与显示**
    search_texts: RefCell<Option<Vec<String>>>,
}

impl EpubBook {
    /// 搜索缓存的失效指纹：size + mtime + 文件头尾各 4KB。
    /// 只看 size/mtime 会漏掉「同样大小、同一毫秒内重新打包」的 EPUB，
    /// 于是搜索一直用旧文本；读 8KB 的代价远小于一次全本解压。
    fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.file_size.hash(&mut hasher);
        self.mtime.hash(&mut hasher);
        let Ok(mut f) = File::open(&self.path) else {
            return hasher.finish();
        };
        let mut head = [0u8; 4096];
        let n = f.read(&mut head).unwrap_or(0);
        head[..n].hash(&mut hasher);
        if self.file_size > 8192 {
            let _ = f.seek(SeekFrom::End(-4096));
            let mut tail = [0u8; 4096];
            let n = f.read(&mut tail).unwrap_or(0);
            tail[..n].hash(&mut hasher);
        }
        hasher.finish()
    }

    pub fn open(path: &Path) -> Result<Self, EpubError> {
        let file =
            File::open(path).map_err(|e| EpubError::Open(format!("{} ({e})", path.display())))?;
        let meta = file.metadata().ok();
        let file_size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut zip = ZipArchive::new(BufReader::new(file))
            .map_err(|e| EpubError::Open(format!("不是合法的 EPUB（zip）：{e}")))?;

        let container = read_entry(&mut zip, "META-INF/container.xml")
            .map_err(|_| EpubError::Structure("缺少 META-INF/container.xml".into()))?;
        let opf_path = parse_container(&container)?;
        let opf_dir = parent_dir(&opf_path);

        let opf = read_entry(&mut zip, &opf_path)
            .map_err(|e| EpubError::Structure(format!("读不到 {opf_path}：{e}")))?;
        let pkg = parse_package(&opf)?;

        // 目录标题优先取 EPUB3 的 nav，其次 EPUB2 的 ncx
        let mut titles: HashMap<String, String> = HashMap::new();
        if let Some(nav_href) = pkg
            .manifest
            .values()
            .find(|i| i.properties.split_whitespace().any(|p| p == "nav"))
            .map(|i| i.href.clone())
        {
            if let Ok(nav) = read_entry(&mut zip, &join_path(&opf_dir, &nav_href)) {
                collect_nav(&nav, &mut titles);
            }
        }
        if titles.is_empty() {
            if let Some(ncx_href) =
                pkg.toc_id.as_ref().and_then(|id| pkg.manifest.get(id)).map(|i| i.href.clone())
            {
                if let Ok(ncx) = read_entry(&mut zip, &join_path(&opf_dir, &ncx_href)) {
                    collect_ncx(&ncx, &mut titles);
                }
            }
        }

        let mut chapters = Vec::new();
        let mut hrefs = Vec::new();
        for idref in &pkg.spine {
            let Some(item) = pkg.manifest.get(idref) else { continue };
            if !is_text_item(&item.media_type) {
                continue;
            }
            let key = strip_fragment(&item.href);
            let title = titles
                .get(&item.href)
                .or_else(|| titles.get(key))
                .cloned()
                .unwrap_or_else(|| format!("第 {} 节", chapters.len() + 1));
            chapters.push(ChapterMeta { index: chapters.len(), title });
            hrefs.push(join_path(&opf_dir, &item.href));
        }

        if chapters.is_empty() {
            return Err(EpubError::Structure("spine 里没有可读的正文".into()));
        }

        let title = if pkg.title.is_empty() {
            path.file_stem().and_then(|s| s.to_str()).unwrap_or("未命名").to_string()
        } else {
            pkg.title
        };

        Ok(EpubBook {
            chapters,
            hrefs,
            archive: RefCell::new(zip),
            title,
            author: pkg.author,
            path: path.to_path_buf(),
            file_size,
            mtime,
            cache_path: crate::text_cache::cache_path(path),
            // 缓存按需加载：不搜索就不读盘，也不占内存
            search_texts: RefCell::new(None),
        })
    }

    pub fn chapters(&self) -> &[ChapterMeta] {
        &self.chapters
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// opf 的 `dc:creator`；没有就 None（TXT 那套文件名嗅探不适用于 EPUB）
    pub fn author(&self) -> Option<&str> {
        self.author.as_deref()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn encoding_name(&self) -> &'static str {
        "UTF-8"
    }

    pub fn confidence(&self) -> f32 {
        1.0
    }

    pub fn chapter_count(&self) -> usize {
        self.chapters.len()
    }

    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    pub fn read_chapter(&self, idx: usize) -> Result<String, EpubError> {
        let href = self
            .hrefs
            .get(idx)
            .ok_or_else(|| EpubError::Structure(format!("章节越界：{idx}")))?
            .clone();
        let html = self.read_entry_text(&href)?;
        Ok(html_to_text(&html))
    }

    /// 整本纯文本，**只给全文搜索用**（显示永远走 `read_chapter`）。
    /// 第一次调用优先读磁盘缓存；没有/过期就逐章解压，并顺手落盘。
    pub(crate) fn texts_for_search(&self) -> std::cell::Ref<'_, Vec<String>> {
        let ready = self.search_texts.borrow().is_some();
        if ready {
            return std::cell::Ref::map(self.search_texts.borrow(), |texts| {
                texts.as_ref().expect("上面刚检查过")
            });
        }

        let stamp = self.fingerprint();
        let chapter_count = self.chapters.len();
        let texts = self
            .cache_path
            .as_deref()
            .and_then(|path| {
                crate::text_cache::load(path, self.file_size, stamp, chapter_count)
            })
            .unwrap_or_else(|| {
                let mut texts = Vec::with_capacity(chapter_count);
                let mut complete = true;
                for i in 0..chapter_count {
                    match self.read_chapter(i) {
                        Ok(t) => texts.push(t),
                        // 某一章读失败就用空串顶上让本次搜索还能跑，但**绝不落盘**：
                        // 否则空章会被固化进缓存，直到 EPUB 改动都搜不到那一章。
                        Err(_) => {
                            complete = false;
                            texts.push(String::new());
                        }
                    }
                }
                if complete {
                    if let Some(path) = &self.cache_path {
                        crate::text_cache::store(path, self.file_size, stamp, &texts);
                    }
                }
                texts
            });

        *self.search_texts.borrow_mut() = Some(texts);
        std::cell::Ref::map(self.search_texts.borrow(), |texts| texts.as_ref().expect("刚填过"))
    }

    fn read_entry_text(&self, name: &str) -> Result<String, EpubError> {
        let mut zip = self.archive.borrow_mut();
        // 有些 epub 的 href 是百分号编码的，直接查不到就解码后再试
        let index = zip
            .index_for_name(name)
            .or_else(|| zip.index_for_name(&percent_decode(name)))
            .ok_or_else(|| EpubError::Io(format!("压缩包里找不到 {name}")))?;
        let mut f = zip.by_index(index).map_err(|e| EpubError::Io(e.to_string()))?;
        let mut buf = Vec::new();
        f.by_ref()
            .take(MAX_ENTRY_BYTES)
            .read_to_end(&mut buf)
            .map_err(|e| EpubError::Io(e.to_string()))?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }
}

// ---------------------------------------------------------------- zip 小工具

fn read_entry(zip: &mut ZipArchive<BufReader<File>>, name: &str) -> Result<String, String> {
    let index = zip
        .index_for_name(name)
        .or_else(|| zip.index_for_name(&percent_decode(name)))
        .ok_or_else(|| format!("找不到 {name}"))?;
    let mut f = zip.by_index(index).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    f.by_ref()
        .take(MAX_ENTRY_BYTES)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------- XML

fn parse_container(xml: &str) -> Result<String, EpubError> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| EpubError::Structure(format!("container.xml 解析失败：{e}")))?;
    doc.descendants()
        .find(|n| n.tag_name().name() == "rootfile")
        .and_then(|n| n.attribute("full-path"))
        .map(|s| s.trim_start_matches('/').to_string())
        .ok_or_else(|| EpubError::Structure("container.xml 里没有 rootfile".into()))
}

fn parse_package(xml: &str) -> Result<Package, EpubError> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| EpubError::Structure(format!("opf 解析失败：{e}")))?;

    let mut pkg = Package {
        title: String::new(),
        author: None,
        manifest: HashMap::new(),
        spine: Vec::new(),
        toc_id: None,
    };

    for n in doc.descendants() {
        match n.tag_name().name() {
            "title" => {
                if pkg.title.is_empty() {
                    if let Some(t) = n.text() {
                        pkg.title = t.trim().to_string();
                    }
                }
            }
            // dc:creator。同一本可能有多个作者，书架只放得下一个，取第一个
            "creator" => {
                if pkg.author.is_none() {
                    if let Some(a) = n.text() {
                        let a = a.trim();
                        if !a.is_empty() {
                            pkg.author = Some(a.to_string());
                        }
                    }
                }
            }
            "item" => {
                let id = n.attribute("id").unwrap_or_default();
                let href = n.attribute("href").unwrap_or_default();
                if !id.is_empty() && !href.is_empty() {
                    pkg.manifest.insert(
                        id.to_string(),
                        Item {
                            href: href.to_string(),
                            media_type: n.attribute("media-type").unwrap_or_default().to_string(),
                            properties: n.attribute("properties").unwrap_or_default().to_string(),
                        },
                    );
                }
            }
            "spine" => {
                if let Some(toc) = n.attribute("toc") {
                    pkg.toc_id = Some(toc.to_string());
                }
            }
            "itemref" => {
                if let Some(idref) = n.attribute("idref") {
                    pkg.spine.push(idref.to_string());
                }
            }
            _ => {}
        }
    }

    Ok(pkg)
}

fn collect_nav(xml: &str, out: &mut HashMap<String, String>) {
    let Ok(doc) = roxmltree::Document::parse(xml) else { return };
    for a in doc.descendants().filter(|n| n.tag_name().name() == "a") {
        let Some(href) = a.attribute("href") else { continue };
        let text = a.text().unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        out.entry(strip_fragment(href).to_string()).or_insert_with(|| text.to_string());
    }
}

fn collect_ncx(xml: &str, out: &mut HashMap<String, String>) {
    let Ok(doc) = roxmltree::Document::parse(xml) else { return };
    for np in doc.descendants().filter(|n| n.tag_name().name() == "navPoint") {
        let title = np
            .descendants()
            .find(|n| n.tag_name().name() == "text")
            .and_then(|n| n.text())
            .unwrap_or("")
            .trim()
            .to_string();
        let src = np
            .descendants()
            .find(|n| n.tag_name().name() == "content")
            .and_then(|n| n.attribute("src"))
            .unwrap_or("")
            .to_string();
        if title.is_empty() || src.is_empty() {
            continue;
        }
        out.entry(strip_fragment(&src).to_string()).or_insert(title);
    }
}

// ---------------------------------------------------------------- 路径

fn parent_dir(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => String::new(),
    }
}

fn strip_fragment(href: &str) -> &str {
    match href.find('#') {
        Some(i) => &href[..i],
        None => href,
    }
}

fn join_path(base: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = if base.is_empty() {
        Vec::new()
    } else {
        base.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn is_text_item(media_type: &str) -> bool {
    let mt = media_type.to_ascii_lowercase();
    mt.contains("xhtml") || mt.contains("html")
}

// ---------------------------------------------------------------- HTML → 文本

// Rust 的 regex crate 不支持反向引用，所以三条规则分开写。
// epub 的 head/style/script 段通常不长，非贪婪匹配足够。
static RE_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?is)<head\b[^>]*>.*?</head\s*>|<style\b[^>]*>.*?</style\s*>|<script\b[^>]*>.*?</script\s*>",
    )
    .unwrap()
});
static RE_BREAK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<\s*(?:br|/p|/div|/h[1-6]|/li|/tr|/blockquote|/section)\b[^>]*>").unwrap()
});
static RE_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());
static RE_ENTITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&(#\d+|#[xX][0-9a-fA-F]+|[a-zA-Z]{2,8});").unwrap());
static RE_BLANK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").unwrap());

pub fn html_to_text(html: &str) -> String {
    let s = RE_HEAD.replace_all(html, "");
    let s = RE_BREAK.replace_all(&s, "\n");
    let s = RE_TAG.replace_all(&s, "");
    let s = decode_entities(&s);
    let s = s.replace('\u{a0}', " ");

    let mut out = String::with_capacity(s.len());
    for line in s.lines() {
        let t = line.trim();
        if t.is_empty() {
            out.push('\n');
        } else {
            out.push_str(t);
            out.push('\n');
        }
    }
    let collapsed = RE_BLANK.replace_all(&out, "\n\n");
    collapsed.trim().to_string()
}

fn decode_entities(s: &str) -> String {
    RE_ENTITY
        .replace_all(s, |caps: &regex::Captures<'_>| {
            let e = &caps[1];
            match e {
                "amp" => "&".into(),
                "lt" => "<".into(),
                "gt" => ">".into(),
                "quot" => "\"".into(),
                "apos" => "'".into(),
                "nbsp" => " ".into(),
                "mdash" => "—".into(),
                "ndash" => "–".into(),
                "hellip" => "…".into(),
                "ldquo" => "“".into(),
                "rdquo" => "”".into(),
                "lsquo" => "‘".into(),
                "rsquo" => "’".into(),
                _ => {
                    if let Some(rest) = e.strip_prefix('#') {
                        let code = match rest.strip_prefix('x').or_else(|| rest.strip_prefix('X')) {
                            Some(hex) => u32::from_str_radix(hex, 16).ok(),
                            None => rest.parse::<u32>().ok(),
                        };
                        if let Some(c) = code.and_then(char::from_u32) {
                            return c.to_string();
                        }
                    }
                    caps[0].to_string()
                }
            }
        })
        .into_owned()
}
