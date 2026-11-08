//! 枚举系统已安装、且能显示中文的字体族名（Windows）。
//!
//! WebView2 直接支持系统字体，只要拿到准确的字体族名（CSS `font-family`），
//! 不需要读取或复制字体文件。这里只扫描系统字体目录的头部、`name` 表和 `cmap` 表，
//! 不依赖 UI、不引入额外 crate；读不到时返回空列表，由前端提示。

use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// 单次读取上限，防御损坏文件把内存吃光
const MAX_READ: usize = 1 << 20;
const MAX_TABLES: usize = 256;
const MAX_TTC_FONTS: usize = 64;
const MAX_NAME_RECORDS: usize = 4096;

/// 列出能显示常用汉字的系统字体族名，已排序、去重。
/// 只读每个字体文件的头部、name 表和 cmap 表，不整文件加载。
pub fn system_font_families() -> Vec<String> {
    let mut names = BTreeSet::new();

    for dir in font_dirs() {
        let Ok(entries) = fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_font_file(&path) {
                continue;
            }
            let Ok(mut file) = fs::File::open(&path) else { continue };
            for family in families_from_font(&mut file) {
                let family = family.trim().to_string();
                if !family.is_empty() && !family.starts_with('.') {
                    names.insert(family);
                }
            }
        }
    }

    names.into_iter().collect()
}

/// 系统字体目录：Windows 共享字体 + 当前用户安装的字体
fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(windir) = std::env::var_os("WINDIR") {
        dirs.push(PathBuf::from(windir).join("Fonts"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Microsoft").join("Windows").join("Fonts"));
    }
    dirs
}

fn is_font_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("ttf" | "otf" | "ttc" | "otc")
    )
}

fn read_at<R: Read + Seek>(file: &mut R, offset: u64, len: usize) -> Option<Vec<u8>> {
    if len > MAX_READ {
        return None;
    }
    let mut buf = vec![0u8; len];
    file.seek(SeekFrom::Start(offset)).ok()?;
    file.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn u16be(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

fn u32be(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// 一个字体文件可能是单个 sfnt，也可能是 .ttc 字体集合
fn families_from_font<R: Read + Seek>(file: &mut R) -> Vec<String> {
    let Some(head) = read_at(file, 0, 12) else { return Vec::new() };

    let mut out = Vec::new();
    if &head[0..4] == b"ttcf" {
        let count = (u32be(&head[8..12]) as usize).min(MAX_TTC_FONTS);
        if count == 0 {
            return out;
        }
        let Some(offsets) = read_at(file, 12, count * 4) else { return out };
        for i in 0..count {
            let off = u32be(&offsets[i * 4..i * 4 + 4]) as u64;
            if let Some(name) = font_family_and_cjk(file, off) {
                out.push(name);
            }
        }
    } else if let Some(name) = font_family_and_cjk(file, 0) {
        out.push(name);
    }
    out
}

/// 读一个 sfnt 的 name 和 cmap。只返回能显示常用汉字的字体。
/// `base` 是文件内偏移（TTC 会大于 0）。
fn font_family_and_cjk<R: Read + Seek>(file: &mut R, base: u64) -> Option<String> {
    let head = read_at(file, base, 12)?;
    let tag = &head[0..4];
    if !(tag == b"\x00\x01\x00\x00" || tag == b"OTTO" || tag == b"true") {
        return None;
    }

    let num_tables = u16be(&head[4..6]) as usize;
    if num_tables == 0 || num_tables > MAX_TABLES {
        return None;
    }

    let dir = read_at(file, base + 12, num_tables * 16)?;
    let mut name = None;
    let mut cmap = None;
    let mut os2 = None;
    for i in 0..num_tables {
        let rec = &dir[i * 16..i * 16 + 16];
        let offset = u32be(&rec[8..12]) as u64;
        let len = u32be(&rec[12..16]) as usize;
        if len > MAX_READ {
            continue;
        }
        // TTC 里的表偏移是相对整个文件的绝对偏移
        match &rec[0..4] {
            b"name" => name = Some((offset, len)),
            b"cmap" => cmap = Some((offset, len)),
            b"OS/2" => os2 = Some((offset, len)),
            _ => {}
        }
    }

    let (name_off, name_len) = name?;
    let (cmap_off, cmap_len) = cmap?;

    // 有代码页信息时只保留声明了中文（GB2312 / Big5）的字体；
    // 没有声明信息的字体退回按字形覆盖判断。
    if let Some((os2_off, os2_len)) = os2 {
        if let Some(data) = read_at(file, os2_off, os2_len) {
            if os2_chinese_codepage(&data) == Some(false) {
                return None;
            }
        }
    }

    let cmap_data = read_at(file, cmap_off, cmap_len)?;
    if !cmap_has_cjk(&cmap_data) {
        return None;
    }

    let name_data = read_at(file, name_off, name_len)?;
    parse_name_table(&name_data)
}

/// OS/2 代码页范围里是否声明了中文。None 表示字体没提供代码页信息，
/// 调用方退回按字形覆盖判断。
fn os2_chinese_codepage(data: &[u8]) -> Option<bool> {
    if data.len() < 82 {
        return None;
    }
    let range1 = u32be(&data[78..82]);
    let range2 = if data.len() >= 86 { u32be(&data[82..86]) } else { 0 };
    if range1 == 0 && range2 == 0 {
        return None;
    }
    // bit 18：简体中文 GB2312；bit 20：繁体中文 Big5
    Some(range1 & 0x0014_0000 != 0)
}

/// 常用汉字探针：这 5 个字都能成字形，才认为字体能用于中文正文
const CJK_PROBES: [u16; 5] = [0x7684, 0x4E00, 0x662F, 0x4E2D, 0x4EBA]; // 的 一 是 中 人

fn cmap_has_cjk(data: &[u8]) -> bool {
    if data.len() < 4 {
        return false;
    }
    let count = (u16be(&data[2..4]) as usize).min(64);
    for i in 0..count {
        let Some(rec) = data.get(4 + i * 8..4 + i * 8 + 8) else { break };
        let platform = u16be(&rec[0..2]);
        let encoding = u16be(&rec[2..4]);
        let offset = u32be(&rec[4..8]) as usize;

        // 只看 Unicode 子表
        if !(platform == 0 || (platform == 3 && matches!(encoding, 1 | 10))) {
            continue;
        }
        let Some(sub) = data.get(offset..) else { continue };
        if sub.len() < 4 {
            continue;
        }

        let ok = match u16be(&sub[0..2]) {
            4 => cmap4_covers_cjk(sub),
            12 => cmap12_covers_cjk(sub),
            _ => false,
        };
        if ok {
            return true;
        }
    }
    false
}

fn cmap4_covers_cjk(sub: &[u8]) -> bool {
    CJK_PROBES.iter().all(|cp| cmap4_lookup(sub, *cp))
}

/// 在 cmap format 4 里查一个 BMP 字符是否有字形（glyph 0 表示没有）
fn cmap4_lookup(sub: &[u8], cp: u16) -> bool {
    if sub.len() < 24 || u16be(&sub[0..2]) != 4 {
        return false;
    }
    let seg_count = (u16be(&sub[6..8]) / 2) as usize;
    if seg_count == 0 || seg_count > 4096 {
        return false;
    }

    let Some(end_codes) = sub.get(14..14 + seg_count * 2) else { return false };
    let start_at = 14 + seg_count * 2 + 2; // 中间有 reservedPad
    let Some(start_codes) = sub.get(start_at..start_at + seg_count * 2) else { return false };
    let delta_at = start_at + seg_count * 2;
    let Some(deltas) = sub.get(delta_at..delta_at + seg_count * 2) else { return false };
    let ro_at = delta_at + seg_count * 2;
    let Some(range_offsets) = sub.get(ro_at..ro_at + seg_count * 2) else { return false };

    for i in 0..seg_count {
        let end = u16be(&end_codes[i * 2..i * 2 + 2]);
        if cp > end {
            continue;
        }
        let start = u16be(&start_codes[i * 2..i * 2 + 2]);
        if cp < start {
            return false;
        }

        let delta = i16::from_be_bytes([deltas[i * 2], deltas[i * 2 + 1]]);
        let ro = u16be(&range_offsets[i * 2..i * 2 + 2]) as usize;
        if ro == 0 {
            return ((cp as i32 + delta as i32) as u16) != 0;
        }

        // idRangeOffset 的地址相对它自己所在位置
        let addr = ro_at + i * 2 + ro + 2 * (cp - start) as usize;
        let Some(bytes) = sub.get(addr..addr + 2) else { return false };
        let glyph = u16be(bytes);
        return glyph != 0 && ((glyph as i32 + delta as i32) as u16) != 0;
    }
    false
}

fn cmap12_covers_cjk(sub: &[u8]) -> bool {
    if sub.len() < 16 || u16be(&sub[0..2]) != 12 {
        return false;
    }
    let groups = u32be(&sub[12..16]) as usize;
    if groups == 0 || groups > 100_000 {
        return false;
    }

    CJK_PROBES.iter().all(|cp| {
        let cp = *cp as u32;
        for i in 0..groups {
            let Some(g) = sub.get(16 + i * 12..16 + i * 12 + 12) else { return false };
            let start = u32be(&g[0..4]);
            let end = u32be(&g[4..8]);
            let glyph = u32be(&g[8..12]);
            if start <= cp && cp <= end {
                return glyph.checked_add(cp - start).is_some_and(|g| g != 0);
            }
        }
        false
    })
}

/// 在 name 表里找族名：优先 typographic family（nameID 16），其次 nameID 1；
/// 同优先级下优先英文 Windows 记录。
fn parse_name_table(data: &[u8]) -> Option<String> {
    if data.len() < 6 {
        return None;
    }
    let count = u16be(&data[2..4]) as usize;
    let storage = u16be(&data[4..6]) as usize;
    if count == 0 || count > MAX_NAME_RECORDS || storage > data.len() {
        return None;
    }

    let mut best: Option<(u8, String)> = None;
    for i in 0..count {
        let start = 6 + i * 12;
        let Some(rec) = data.get(start..start + 12) else { break };

        let platform = u16be(&rec[0..2]);
        let encoding = u16be(&rec[2..4]);
        let lang = u16be(&rec[4..6]);
        let name_id = u16be(&rec[6..8]);
        let len = u16be(&rec[8..10]) as usize;
        let off = u16be(&rec[10..12]) as usize;

        if name_id != 1 && name_id != 16 {
            continue;
        }
        // 只要 Unicode 记录（Windows UCS-2 或 Unicode 平台）
        if !(platform == 0 || (platform == 3 && matches!(encoding, 1 | 10))) {
            continue;
        }

        let Some(start) = storage.checked_add(off) else { continue };
        let Some(end) = start.checked_add(len) else { continue };
        let Some(bytes) = data.get(start..end) else { continue };

        let name = decode_utf16be(bytes)
            .trim_matches(|c: char| c == '\0' || c.is_whitespace())
            .to_string();
        if name.is_empty() {
            continue;
        }

        let score = (if name_id == 16 { 2 } else { 0 })
            + (if lang == 0x0409 { 1 } else { 0 })
            + (if platform == 3 { 1 } else { 0 });
        if best.as_ref().is_none_or(|(best_score, _)| score > *best_score) {
            best = Some((score, name));
        }
    }

    best.map(|(_, name)| name)
}

fn decode_utf16be(bytes: &[u8]) -> String {
    let units: Vec<u16> =
        bytes.as_chunks::<2>().0.iter().map(|p| u16::from_be_bytes(*p)).collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn utf16be(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
    }

    /// 造一个 format 4 的 cmap，单个段 [start, end]，字形 = 码位 + 1
    fn format4_cmap(start: u16, end: u16) -> Vec<u8> {
        let mut sub = Vec::new();
        sub.extend_from_slice(&4u16.to_be_bytes()); // format
        sub.extend_from_slice(&24u16.to_be_bytes()); // length
        sub.extend_from_slice(&0u16.to_be_bytes()); // language
        sub.extend_from_slice(&2u16.to_be_bytes()); // segCountX2
        sub.extend_from_slice(&0u16.to_be_bytes()); // searchRange
        sub.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
        sub.extend_from_slice(&0u16.to_be_bytes()); // rangeShift
        sub.extend_from_slice(&end.to_be_bytes()); // endCode
        sub.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
        sub.extend_from_slice(&start.to_be_bytes()); // startCode
        sub.extend_from_slice(&1i16.to_be_bytes()); // idDelta
        sub.extend_from_slice(&0u16.to_be_bytes()); // idRangeOffset

        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes()); // version
        cmap.extend_from_slice(&1u16.to_be_bytes()); // numTables
        cmap.extend_from_slice(&3u16.to_be_bytes()); // platform Windows
        cmap.extend_from_slice(&1u16.to_be_bytes()); // encoding UCS-2
        cmap.extend_from_slice(&12u32.to_be_bytes()); // subtable offset
        cmap.extend_from_slice(&sub);
        cmap
    }

    /// OS/2 表：version 1，只设置 `range1`（代码页范围 1）
    fn os2_table(range1: u32) -> Vec<u8> {
        let mut t = vec![0u8; 90];
        t[0..2].copy_from_slice(&1u16.to_be_bytes());
        t[78..82].copy_from_slice(&range1.to_be_bytes());
        t
    }

    /// 造一个迷你 sfnt；`base` 是它将落在文件里的起始偏移（TTC 放头部之后传 16）。
    /// cmap / OS/2 传 None 表示没有该表。
    fn font_with_names(
        base: usize,
        records: &[(u16, u16, &str)],
        cmap: Option<&[u8]>,
        os2: Option<&[u8]>,
    ) -> Vec<u8> {
        let mut strings = Vec::new();
        let mut name_records = Vec::new();
        for (name_id, lang, text) in records {
            let bytes = utf16be(text);
            name_records.push((*name_id, *lang, strings.len() as u16, bytes.len() as u16));
            strings.extend_from_slice(&bytes);
        }

        let mut name = Vec::new();
        name.extend_from_slice(&0u16.to_be_bytes());
        name.extend_from_slice(&(name_records.len() as u16).to_be_bytes());
        name.extend_from_slice(&((6 + name_records.len() * 12) as u16).to_be_bytes());
        for (id, lang, off, len) in &name_records {
            name.extend_from_slice(&3u16.to_be_bytes()); // Windows
            name.extend_from_slice(&1u16.to_be_bytes()); // UCS-2
            name.extend_from_slice(&lang.to_be_bytes());
            name.extend_from_slice(&id.to_be_bytes());
            name.extend_from_slice(&len.to_be_bytes());
            name.extend_from_slice(&off.to_be_bytes());
        }
        name.extend_from_slice(&strings);

        let mut tables: Vec<([u8; 4], Vec<u8>)> = vec![(*b"name", name)];
        if let Some(c) = cmap {
            tables.push((*b"cmap", c.to_vec()));
        }
        if let Some(o) = os2 {
            tables.push((*b"OS/2", o.to_vec()));
        }
        tables.sort_by_key(|t| t.0);

        let mut offsets = Vec::with_capacity(tables.len());
        let mut cursor = base + 12 + tables.len() * 16;
        for (_, data) in &tables {
            offsets.push(cursor);
            cursor += data.len();
        }

        let mut out = Vec::new();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
        out.extend_from_slice(&[0u8; 6]); // searchRange / entrySelector / rangeShift
        for ((tag, data), off) in tables.iter().zip(offsets.iter()) {
            out.extend_from_slice(tag);
            out.extend_from_slice(&0u32.to_be_bytes()); // checksum
            out.extend_from_slice(&(*off as u32).to_be_bytes());
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        }
        for (_, data) in &tables {
            out.extend_from_slice(data);
        }
        out
    }

    #[test]
    fn reads_chinese_family_from_sfnt() {
        let cmap = format4_cmap(0x4E00, 0x9FFF);
        let bytes = font_with_names(0, &[(1, 0x0409, "Test Family")], Some(&cmap), None);
        let mut cursor = Cursor::new(&bytes);
        assert_eq!(font_family_and_cjk(&mut cursor, 0).as_deref(), Some("Test Family"));
    }

    #[test]
    fn prefers_typographic_family() {
        let cmap = format4_cmap(0x4E00, 0x9FFF);
        let bytes = font_with_names(
            0,
            &[(1, 0x0409, "Old Name"), (16, 0x0409, "Real Family")],
            Some(&cmap),
            None,
        );
        let mut cursor = Cursor::new(&bytes);
        assert_eq!(font_family_and_cjk(&mut cursor, 0).as_deref(), Some("Real Family"));
    }

    #[test]
    fn latin_only_font_is_excluded() {
        let cmap = format4_cmap(0x0041, 0x005A);
        let bytes = font_with_names(0, &[(1, 0x0409, "Latin Only")], Some(&cmap), None);
        let mut cursor = Cursor::new(&bytes);
        assert_eq!(font_family_and_cjk(&mut cursor, 0), None);
    }

    #[test]
    fn font_without_cmap_is_excluded() {
        let bytes = font_with_names(0, &[(1, 0x0409, "No Cmap")], None, None);
        let mut cursor = Cursor::new(&bytes);
        assert_eq!(font_family_and_cjk(&mut cursor, 0), None);
    }

    #[test]
    fn chinese_codepage_is_included() {
        let cmap = format4_cmap(0x4E00, 0x9FFF);
        let os2 = os2_table(1 << 18); // GB2312
        let bytes = font_with_names(
            0,
            &[(1, 0x0409, "Chinese Font")],
            Some(&cmap),
            Some(&os2),
        );
        let mut cursor = Cursor::new(&bytes);
        assert_eq!(font_family_and_cjk(&mut cursor, 0).as_deref(), Some("Chinese Font"));
    }

    #[test]
    fn japanese_codepage_is_excluded() {
        let cmap = format4_cmap(0x4E00, 0x9FFF);
        let os2 = os2_table(1 << 17); // JIS
        let bytes = font_with_names(
            0,
            &[(1, 0x0409, "Japanese Font")],
            Some(&cmap),
            Some(&os2),
        );
        let mut cursor = Cursor::new(&bytes);
        assert_eq!(font_family_and_cjk(&mut cursor, 0), None);
    }

    #[test]
    fn reads_chinese_family_from_ttc() {
        let cmap = format4_cmap(0x4E00, 0x9FFF);
        let font = font_with_names(
            16,
            &[(1, 0x0409, "Collection Family")],
            Some(&cmap),
            None,
        );
        let mut ttc = Vec::new();
        ttc.extend_from_slice(b"ttcf");
        ttc.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        ttc.extend_from_slice(&1u32.to_be_bytes());
        ttc.extend_from_slice(&16u32.to_be_bytes());
        ttc.extend_from_slice(&font);

        let mut cursor = Cursor::new(&ttc);
        assert_eq!(families_from_font(&mut cursor), vec!["Collection Family".to_string()]);
    }

    #[test]
    fn malformed_font_does_not_panic() {
        let empty: &[u8] = b"";
        let junk: &[u8] = b"not a font";
        let short_header: &[u8] = &[0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        for bytes in [empty, junk, short_header] {
            let mut cursor = Cursor::new(bytes);
            assert!(families_from_font(&mut cursor).is_empty());
        }
    }
}
