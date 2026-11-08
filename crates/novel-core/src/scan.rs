//! 递归扫描一个文件夹里的 txt / epub，供「导入文件夹」按磁盘目录建分组。
//!
//! 只做文件系统遍历，不碰数据库、不依赖 Tauri：扫描结果交给
//! `store::ensure_folder` + 导入流程去建目录和写库。

use std::path::{Path, PathBuf};

/// 最多往下扫这么多层。正常小说目录不会超过，设上限是为了防 junction / 深层嵌套。
const MAX_SCAN_DEPTH: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedEbook {
    pub path: PathBuf,
    /// 相对扫描根的目录分量（不含根本身）。根目录下的文件是空 Vec。
    pub dirs: Vec<String>,
}

/// 递归扫描 `root` 下的 txt / epub（扩展名大小写不敏感）。
/// 跳过隐藏目录、符号链接、回收站 / 系统卷目录、Office 临时文件（`~$`）。
/// 结果按路径排序，保证多次导入的顺序稳定。
pub fn scan_ebooks(root: &Path) -> Vec<ScannedEbook> {
    let mut out = Vec::new();
    let mut dirs = Vec::new();
    walk(root, &mut dirs, &mut out, 0);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn walk(dir: &Path, dirs: &mut Vec<String>, out: &mut Vec<ScannedEbook>, depth: usize) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // symlink_metadata 不跟随链接：junction 指回上层会无限递归
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }

        if meta.is_dir() {
            if should_skip_dir(name) {
                continue;
            }
            dirs.push(name.to_string());
            walk(&path, dirs, out, depth + 1);
            dirs.pop();
        } else if meta.is_file() && is_ebook(&path) && !name.starts_with("~$") {
            out.push(ScannedEbook { path: path.clone(), dirs: dirs.clone() });
        }
    }
}

fn is_ebook(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("txt") | Some("epub")
    )
}

fn should_skip_dir(name: &str) -> bool {
    name.starts_with('.')
        || name.eq_ignore_ascii_case("$RECYCLE.BIN")
        || name.eq_ignore_ascii_case("System Volume Information")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "novel-scan-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scans_nested_txt_and_epub_case_insensitively() {
        let root = temp_root("nested");
        fs::create_dir_all(root.join("玄幻").join("仙侠")).unwrap();
        fs::write(root.join("根目录.txt"), "x").unwrap();
        fs::write(root.join("玄幻").join("剑来.TXT"), "x").unwrap();
        fs::write(root.join("玄幻").join("仙侠").join("凡人.EPUB"), "x").unwrap();
        fs::write(root.join("玄幻").join("封面.jpg"), "x").unwrap();
        fs::write(root.join("玄幻").join("说明.md"), "x").unwrap();

        let found = scan_ebooks(&root);
        let names: Vec<String> = found
            .iter()
            .map(|b| b.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"根目录.txt".to_string()));

        let fanren = found.iter().find(|b| b.path.ends_with("凡人.EPUB")).unwrap();
        assert_eq!(fanren.dirs, vec!["玄幻".to_string(), "仙侠".to_string()]);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn skips_hidden_dirs_and_office_temp_files() {
        let root = temp_root("skip");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git").join("藏在仓库里.txt"), "x").unwrap();
        fs::write(root.join("~$草稿.txt"), "x").unwrap();
        fs::write(root.join("正常.txt"), "x").unwrap();

        let found = scan_ebooks(&root);
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("正常.txt"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_root_returns_empty() {
        let found = scan_ebooks(Path::new("Z:\\definitely-not-here"));
        assert!(found.is_empty());
    }
}
