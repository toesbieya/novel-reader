//! Tauri 命令层：把 novel-core 的数据层暴露给前端。
//!
//! 这一层只做三件事：拿锁、调引擎、把错误转成字符串。
//! 任何业务规则都不写在这里——规则属于 `novel-core`，方便脱离 UI 测试。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use novel_core::model::ChapterMeta;
use novel_core::store::{
    BookSummary, ChapterContent, DeleteOutcome, Folder, ImportOutcome, Library, Progress,
    ReindexNotice, SearchResult,
};
use novel_core::Document;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, PhysicalPosition, PhysicalSize};

pub struct AppState {
    lib: Mutex<Library>,
    /// 当前书库 / 设置所在目录（可在设置里迁移）
    data_dir: PathBuf,
    /// 默认目录：放 data-dir.txt 指针、迁移旧的 .window-state.json
    default_dir: PathBuf,
    /// 导入批次取消标记。每本书之间检查一次，正在切的那本会做完——
    /// 半本书已经解完再丢掉不值当，而且 commit_import 是事务，本来就原子。
    import_cancel: AtomicBool,
}

type R<T> = Result<T, String>;

fn e<E: std::fmt::Display>(v: E) -> String {
    v.to_string()
}

/// 窗口状态在 setting 表里的 key
const WINDOW_STATE_KEY: &str = "window.state";
/// 自定义数据目录的指针文件，放在默认 AppData 目录
const DATA_DIR_POINTER: &str = "data-dir.txt";

/// 关闭时保存的窗口几何（物理像素）
#[derive(Clone, Serialize, Deserialize)]
struct WindowState {
    /// 内容区宽度
    width: u32,
    /// 内容区高度
    height: u32,
    /// 窗口左上角 X
    x: i32,
    /// 窗口左上角 Y
    y: i32,
    /// 关闭时是否最大化
    maximized: bool,
}

/// 数据目录优先级：环境变量 → 指针文件 → 默认 AppData 目录
fn resolve_data_dir(default_dir: &Path) -> PathBuf {
    if let Some(raw) = std::env::var_os("NOVEL_READER_DATA_DIR") {
        let path = PathBuf::from(raw);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    let pointer = default_dir.join(DATA_DIR_POINTER);
    if let Ok(raw) = fs::read_to_string(&pointer) {
        let path = PathBuf::from(raw.trim());
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    default_dir.to_path_buf()
}

/// 解析并确保目录可用；自定义目录不可用时退回默认目录，别让应用起不来
fn usable_data_dir(default_dir: &Path) -> PathBuf {
    let dir = resolve_data_dir(default_dir);
    if fs::create_dir_all(&dir).is_ok() {
        return dir;
    }
    eprintln!("[data-dir] 无法使用 {}，回退 {}", dir.display(), default_dir.display());
    let _ = fs::create_dir_all(default_dir);
    default_dir.to_path_buf()
}

/// 优先读数据库；没有就把旧 window-state 插件的 json 迁进来
fn load_window_state(lib: &Library, default_dir: &Path) -> Option<WindowState> {
    let legacy = default_dir.join(".window-state.json");
    if let Ok(Some(raw)) = lib.get_setting(WINDOW_STATE_KEY) {
        let _ = fs::remove_file(&legacy);
        return serde_json::from_str(&raw).ok();
    }

    let raw = fs::read_to_string(&legacy).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let main = value.get("main")?;
    let state = WindowState {
        width: main.get("width")?.as_u64()? as u32,
        height: main.get("height")?.as_u64()? as u32,
        x: main.get("x")?.as_i64()? as i32,
        y: main.get("y")?.as_i64()? as i32,
        maximized: main.get("maximized").and_then(|v| v.as_bool()).unwrap_or(false),
    };

    if let Ok(json) = serde_json::to_string(&state) {
        let _ = lib.set_setting(WINDOW_STATE_KEY, &json);
    }
    let _ = fs::remove_file(&legacy);
    Some(state)
}

fn apply_window_state(window: &tauri::WebviewWindow, state: &WindowState) {
    let _ = window.set_size(PhysicalSize::new(state.width.max(320), state.height.max(240)));
    let _ = window.set_position(PhysicalPosition::new(state.x, state.y));
    if state.maximized {
        let _ = window.maximize();
    }
}

/// 最大化时内容区是最大化后的尺寸，保留上次的普通尺寸，只更新最大化标记
fn current_window_state(
    window: &tauri::WebviewWindow,
    prev: Option<WindowState>,
) -> Option<WindowState> {
    let maximized = window.is_maximized().unwrap_or(false);
    if maximized {
        if let Some(mut state) = prev {
            state.maximized = true;
            return Some(state);
        }
    }

    let size = window.inner_size().ok()?;
    let pos = window.outer_position().ok()?;
    Some(WindowState {
        width: size.width,
        height: size.height,
        x: pos.x,
        y: pos.y,
        maximized,
    })
}

/// 把当前窗口几何写回 SQLite（关窗、迁移数据目录前调用）
fn persist_window_state(app: &tauri::AppHandle, window: &tauri::WebviewWindow) {
    let Some(state) = app.try_state::<AppState>() else { return };
    let Ok(lib) = state.lib.try_lock() else { return };
    let prev = lib
        .get_setting(WINDOW_STATE_KEY)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<WindowState>(&raw).ok());
    if let Some(next) = current_window_state(window, prev) {
        if let Ok(raw) = serde_json::to_string(&next) {
            let _ = lib.set_setting(WINDOW_STATE_KEY, &raw);
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub imported: Vec<ImportOutcome>,
    /// 成功的进书架，失败的逐条回报，不因为一个坏文件中断整批导入
    pub failed: Vec<String>,
    /// 用户点了「取消」：剩余文件没有处理，已完成的都在 imported 里
    pub cancelled: bool,
    /// 「导入文件夹」新建的分组目录数；普通文件导入恒为 0
    pub folders_created: usize,
}

// ---------------------------------------------------------------- 目录

#[tauri::command]
fn list_folders(state: tauri::State<'_, AppState>) -> R<Vec<Folder>> {
    state.lib.lock().unwrap_or_else(|p| p.into_inner()).list_folders().map_err(e)
}

#[tauri::command]
fn create_folder(
    state: tauri::State<'_, AppState>,
    parent_id: Option<i64>,
    name: String,
) -> R<i64> {
    state.lib.lock().unwrap_or_else(|p| p.into_inner()).create_folder(parent_id, &name).map_err(e)
}

#[tauri::command]
fn rename_folder(state: tauri::State<'_, AppState>, id: i64, name: String) -> R<()> {
    state.lib.lock().unwrap_or_else(|p| p.into_inner()).rename_folder(id, &name).map_err(e)
}

#[tauri::command]
fn move_folder(state: tauri::State<'_, AppState>, id: i64, parent_id: Option<i64>) -> R<()> {
    state.lib.lock().unwrap_or_else(|p| p.into_inner()).move_folder(id, parent_id).map_err(e)
}

#[tauri::command]
fn reorder_folder(
    state: tauri::State<'_, AppState>,
    id: i64,
    target_id: i64,
    after: bool,
) -> R<()> {
    state.lib.lock().unwrap_or_else(|p| p.into_inner()).reorder_folder(id, target_id, after).map_err(e)
}

#[tauri::command]
fn delete_folder(state: tauri::State<'_, AppState>, id: i64) -> R<DeleteOutcome> {
    state.lib.lock().unwrap_or_else(|p| p.into_inner()).delete_folder(id).map_err(e)
}

// ---------------------------------------------------------------- 书籍

#[tauri::command]
fn list_books(state: tauri::State<'_, AppState>) -> R<Vec<BookSummary>> {
    state.lib.lock().unwrap_or_else(|p| p.into_inner()).list_books().map_err(e)
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportProgress {
    done: usize,
    total: usize,
    /// 刚处理完的那本书（成功用书名，失败用文件名）
    title: String,
    ok: bool,
}

/// 单本书的导入：查重（锁内，只查一条索引）→ 解析（锁外，占 99% 的时间）
/// → 写库（锁内，一次事务）。
///
/// 解析（打开 + 切章 + 算全书字数，30MB 约 0.3s）必须在锁外：否则导入期间
/// 翻页、存进度这些命令会一起排在 `Mutex<Library>` 后面，界面看起来就是卡住。
fn import_one(app: &tauri::AppHandle, path: &str, folder_id: Option<i64>) -> R<ImportOutcome> {
    let p = std::path::Path::new(path);

    // 已导入就不必再切一遍章；注意 guard 要在块结束时释放，不能带进下面的重活
    {
        let state = app.state::<AppState>();
        let lib = state.lib.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(out) = lib.already_imported(p).map_err(e)? {
            return Ok(out);
        }
    }

    let prepared = Library::prepare_import(p).map_err(e)?;

    let state = app.state::<AppState>();
    let mut lib = state.lib.lock().unwrap_or_else(|p| p.into_inner());
    lib.commit_import(prepared, folder_id).map_err(e)
}

/// 取消当前导入批次。只置一个标记，导入线程在下一本书开始前自行退出，
/// 已经导入的书保持导入状态（书架上看得到），不会回滚。
#[tauri::command]
fn cancel_import(state: tauri::State<'_, AppState>) {
    state.import_cancel.store(true, Ordering::SeqCst);
}

/// 导入。**不能写成同步命令**——Tauri 的同步命令跑在主线程上，
/// 66 本导入时窗口消息循环会被整个堵住，看起来就是"卡死 2-3 秒"。
///
/// 这里丢进阻塞线程池，每本书只在写库时短暂拿锁：正文读取、进度保存这些命令
/// 全程都能插进来，界面可用。
#[tauri::command]
async fn import_paths(
    app: tauri::AppHandle,
    paths: Vec<String>,
    folder_id: Option<i64>,
) -> R<ImportReport> {
    let total = paths.len();

    tauri::async_runtime::spawn_blocking(move || {
        let mut imported = Vec::new();
        let mut failed = Vec::new();
        let mut cancelled = false;

        // 上一批可能留下 true，开新批次前先复位
        app.state::<AppState>().import_cancel.store(false, Ordering::SeqCst);

        for (i, p) in paths.iter().enumerate() {
            if app.state::<AppState>().import_cancel.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }

            let (title, ok) = match import_one(&app, p, folder_id) {
                Ok(out) => {
                    let title = out.title.clone();
                    imported.push(out);
                    (title, true)
                }
                Err(err) => {
                    failed.push(format!("{p}：{err}"));
                    (file_name(p), false)
                }
            };

            let _ = app.emit("import-progress", ImportProgress { done: i + 1, total, title, ok });
        }

        ImportReport { imported, failed, cancelled, folders_created: 0 }
    })
    .await
    .map_err(|err| format!("导入线程异常：{err}"))
}

fn file_name(p: &str) -> String {
    p.rsplit(['\\', '/']).next().unwrap_or(p).to_string()
}

/// 导入整个文件夹：递归扫描 txt / epub，按磁盘上的子目录建分组。
///
/// 同级同名目录用 `ensure_folder` 合并，重复导入同一棵树不会堆出重复分组；
/// 目录最多 5 级，超出的层级拍平到最后一级（文件不会丢）。
/// 取消 / 进度 / 查重全部复用 `import_paths` 那套。
#[tauri::command]
async fn import_folder(
    app: tauri::AppHandle,
    path: String,
    folder_id: Option<i64>,
) -> R<ImportReport> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = std::path::Path::new(&path);
        if !root.is_dir() {
            return Err(format!("不是文件夹：{path}"));
        }

        let files = novel_core::scan::scan_ebooks(root);
        let total = files.len();
        let mut imported = Vec::new();
        let mut failed = Vec::new();
        let mut cancelled = false;
        let mut folders_created = 0usize;

        app.state::<AppState>().import_cancel.store(false, Ordering::SeqCst);
        // 扫描完先报一次总数：目录很大时，后面每本一条进度，用户知道还剩多少
        let _ = app.emit(
            "import-progress",
            ImportProgress { done: 0, total, title: format!("扫描到 {total} 个文件"), ok: true },
        );

        for (i, file) in files.iter().enumerate() {
            if app.state::<AppState>().import_cancel.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }

            // 逐级建组：ensure_folder 在同一父级下按名字复用，重复导入会合并
            let dest = {
                let state = app.state::<AppState>();
                let mut lib = state.lib.lock().unwrap_or_else(|p| p.into_inner());
                let mut parent = folder_id;
                for name in &file.dirs {
                    match lib.ensure_folder(parent, name) {
                        Ok((id, created)) => {
                            if created {
                                folders_created += 1;
                            }
                            parent = Some(id);
                        }
                        // 到 5 级上限（或目录名不合法）：剩下的层级拍平到当前目录
                        Err(_) => break,
                    }
                }
                parent
            };

            let path_text = file.path.to_string_lossy().to_string();
            let (title, ok) = match import_one(&app, &path_text, dest) {
                Ok(out) => {
                    let title = out.title.clone();
                    imported.push(out);
                    (title, true)
                }
                Err(err) => {
                    failed.push(format!("{path_text}：{err}"));
                    (file_name(&path_text), false)
                }
            };

            let _ = app.emit("import-progress", ImportProgress { done: i + 1, total, title, ok });
        }

        Ok(ImportReport { imported, failed, cancelled, folders_created })
    })
    .await
    .map_err(|err| format!("导入线程异常：{err}"))?
}

/// 给升级前入库的老书补章节字数。
///
/// 解码放在锁外面（一本书解完再拿锁写回），所以即使 66 本要跑两秒，
/// 期间打开书、翻页也不会被这把锁挡住。
#[tauri::command]
async fn backfill_char_lens(app: tauri::AppHandle) -> R<usize> {
    tauri::async_runtime::spawn_blocking(move || {
        let pending = {
            let state = app.state::<AppState>();
            let lib = state.lib.lock().unwrap_or_else(|p| p.into_inner());
            lib.char_lens_pending().map_err(e)?
        };

        let mut done = 0usize;

        for (id, path) in pending {
            let p = std::path::Path::new(&path);
            if !p.is_file() {
                continue;
            }
            let Ok(doc) = novel_core::Document::open(p) else {
                continue;
            };

            let lens: Vec<i64> = (0..doc.chapter_count())
                .map(|i| doc.read_chapter(i).map(|t| t.chars().count() as i64).unwrap_or(0))
                .collect();

            {
                let state = app.state::<AppState>();
                let mut lib = state.lib.lock().unwrap_or_else(|p| p.into_inner());
                lib.set_char_lens(id, &lens).map_err(e)?;
            }

            done += 1;
        }

        Ok(done)
    })
    .await
    .map_err(|err| format!("回填线程异常：{err}"))?
}

#[tauri::command]
fn delete_book(state: tauri::State<'_, AppState>, id: i64) -> R<()> {
    state.lib.lock().unwrap_or_else(|p| p.into_inner()).delete_book(id).map_err(e)
}

#[tauri::command]
fn set_book_folder(
    state: tauri::State<'_, AppState>,
    book_id: i64,
    folder_id: Option<i64>,
) -> R<()> {
    state.lib.lock().unwrap_or_else(|p| p.into_inner()).set_book_folder(book_id, folder_id).map_err(e)
}

// ---------------------------------------------------------------- 阅读

#[tauri::command]
fn list_chapters(state: tauri::State<'_, AppState>, book_id: i64) -> R<Vec<ChapterMeta>> {
    state.lib.lock().unwrap_or_else(|p| p.into_inner()).chapters(book_id).map_err(e)
}

/// 数据层攒下的"文件被外部改动"通知，推给前端。
/// 重建索引发生在 `Library` 内部（读章节 / 搜索时才发现），Tauri 层只负责转发。
fn emit_reindex_notices(app: &tauri::AppHandle, notices: Vec<ReindexNotice>) {
    for notice in notices {
        let _ = app.emit("book-reindexed", notice);
    }
}

/// 在锁外完成「外部改动 → 重建章节索引」的重活。
/// 三段式：短锁 stat 拿计划 → 锁外 `Document::open`（全本切章几十毫秒）→ 短锁回写。
/// 以前 `Library::document()` 在锁内 open，一次外部改动就能挡住所有翻页 / 存进度。
fn reindex_outside_lock(app: &AppState, book_id: i64) -> Result<(), String> {
    // 锁被污染时取回内容继续用：本地应用里「一次 panic 后 25 个命令全瘫」更糟。
    let plan = {
        let mut lib = app.lib.lock().unwrap_or_else(|p| p.into_inner());
        lib.reindex_plan(book_id).map_err(e)?
    };
    let Some(plan) = plan else { return Ok(()) };
    let doc = Document::open(&plan.path).map_err(e)?;
    let mut lib = app.lib.lock().unwrap_or_else(|p| p.into_inner());
    lib.reindex_apply(book_id, plan, doc).map_err(e)?;
    Ok(())
}

/// 读一章。可能触发「文件被外部改动 → 重建索引」（几十毫秒级），
/// 所以放阻塞线程池：同步命令跑在主线程，重建时界面会僵一下。
#[tauri::command]
async fn read_chapter(app: tauri::AppHandle, book_id: i64, index: usize) -> R<ChapterContent> {
    let handle = app.clone();
    let (result, notices) = tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        reindex_outside_lock(&state, book_id)?;
        let mut lib = state.lib.lock().unwrap_or_else(|p| p.into_inner());
        let result = lib.read_chapter(book_id, index);
        // 就算这一章读失败（改版后章节数变少），重建索引也该让用户知道
        let notices = lib.take_reindex_notices();
        Ok::<_, String>((result, notices))
    })
    .await
    .map_err(|err| format!("读取章节线程异常：{err}"))??;

    emit_reindex_notices(&app, notices);
    result.map_err(e)
}

/// 搜索。TXT 是 4ms 级，但 EPUB 冷缓存首次要 180ms+，
/// 同步跑在主线程就是肉眼可见的卡顿。
#[tauri::command]
async fn search(
    app: tauri::AppHandle,
    book_id: i64,
    query: String,
    limit: Option<usize>,
    offset: Option<usize>,
) -> R<SearchResult> {
    let handle = app.clone();
    let (result, notices) = tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        reindex_outside_lock(&state, book_id)?;
        let mut lib = state.lib.lock().unwrap_or_else(|p| p.into_inner());
        let result = lib.search_paged(book_id, &query, limit.unwrap_or(200), offset.unwrap_or(0));
        let notices = lib.take_reindex_notices();
        Ok::<_, String>((result, notices))
    })
    .await
    .map_err(|err| format!("搜索线程异常：{err}"))??;

    emit_reindex_notices(&app, notices);
    result.map_err(e)
}

#[tauri::command]
fn save_progress(
    state: tauri::State<'_, AppState>,
    book_id: i64,
    chapter_index: i64,
    char_offset: i64,
) -> R<()> {
    state.lib.lock().unwrap_or_else(|p| p.into_inner()).save_progress(book_id, chapter_index, char_offset).map_err(e)
}

#[tauri::command]
fn get_progress(state: tauri::State<'_, AppState>, book_id: i64) -> R<Option<Progress>> {
    state.lib.lock().unwrap_or_else(|p| p.into_inner()).progress(book_id).map_err(e)
}

// ---------------------------------------------------------------- 文件状态

#[tauri::command]
async fn refresh_missing(app: tauri::AppHandle) -> R<usize> {
    // 逐文件 stat 是慢 IO，别占着主线程；同步命令会把窗口消息循环一起堵住。
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        let mut lib = state.lib.lock().unwrap_or_else(|p| p.into_inner());
        lib.refresh_missing().map_err(e)
    })
    .await
    .map_err(|err| format!("刷新文件状态线程异常：{err}"))?
}

// ---------------------------------------------------------------- 设置 / 数据目录

#[tauri::command]
fn get_setting(state: tauri::State<'_, AppState>, key: String) -> R<Option<String>> {
    state.lib.lock().unwrap_or_else(|p| p.into_inner()).get_setting(&key).map_err(e)
}

#[tauri::command]
fn set_setting(state: tauri::State<'_, AppState>, key: String, value: String) -> R<()> {
    state.lib.lock().unwrap_or_else(|p| p.into_inner()).set_setting(&key, &value).map_err(e)
}

#[tauri::command]
fn data_dir(state: tauri::State<'_, AppState>) -> R<String> {
    Ok(state.data_dir.to_string_lossy().to_string())
}

/// 在资源管理器里打开数据目录，方便用户确认书库位置、手动备份。
/// 只启动 explorer，不碰文件内容。
#[tauri::command]
fn open_data_dir(state: tauri::State<'_, AppState>) -> R<()> {
    std::process::Command::new("explorer")
        .arg(&state.data_dir)
        .spawn()
        .map_err(|err| format!("打开目录失败：{err}"))?;
    Ok(())
}

#[tauri::command]
fn has_library(path: String) -> bool {
    Path::new(&path).join("library.db").is_file()
}

/// 把当前书库迁移到新目录并重启；`use_existing` 为真时直接切到目标已有的书库。
/// 只复制、不覆盖、不删除原目录。
#[tauri::command]
async fn change_data_dir(
    app: tauri::AppHandle,
    new_dir: String,
    use_existing: bool,
) -> R<()> {
    // async 命令里不持有 State<'_> 引用；进阻塞线程前先把两个路径拷出来。
    // VACUUM 全库是慢 IO，以前同步持锁跑主线程，点「迁移数据目录」会明显卡一下。
    let (data_dir, default_dir) = {
        let state = app.state::<AppState>();
        (state.data_dir.clone(), state.default_dir.clone())
    };
    let target = PathBuf::from(new_dir.trim());
    if !target.is_absolute() {
        return Err("请选择绝对路径的数据目录".into());
    }
    if target == data_dir {
        return Ok(());
    }

    fs::create_dir_all(&target).map_err(|err| format!("创建数据目录失败：{err}"))?;
    let dest_db = target.join("library.db");
    let has_db = dest_db.exists();
    if has_db && !use_existing {
        return Err("目标目录里已经有 library.db；需要切换过去请在界面上确认".into());
    }

    if !has_db {
        // 把当前窗口几何先写进库，保证复制出去的 db 也包含它
        if let Some(window) = app.get_webview_window("main") {
            persist_window_state(&app, &window);
        }
        let dest = dest_db.clone();
        let handle = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let state = handle.state::<AppState>();
            let lib = state.lib.lock().unwrap_or_else(|p| p.into_inner());
            lib.copy_db_to(&dest).map_err(e)
        })
        .await
        .map_err(|err| format!("复制书库线程异常：{err}"))??;
    }

    let pointer = default_dir.join(DATA_DIR_POINTER);
    if target == default_dir {
        let _ = fs::remove_file(&pointer);
    } else {
        fs::write(&pointer, target.to_string_lossy().as_bytes())
            .map_err(|err| format!("写入数据目录指针失败：{err}"))?;
    }

    // 从新目录重新打开数据库；这里不会返回
    app.restart();
}

/// 枚举系统已安装的字体族名。前端拿到名字后直接写 CSS `font-family`，
/// 不需要读字体文件、也不复制。
#[tauri::command]
async fn list_system_fonts() -> R<Vec<String>> {
    tauri::async_runtime::spawn_blocking(novel_core::fonts::system_font_families)
        .await
        .map_err(|err| format!("枚举系统字体异常：{err}"))
}

/// 读字体文件给前端注册 `@font-face`。
///
/// 必须走 `ipc::Response` 的原始字节通道：直接返回 `Vec<u8>` 会被序列化成
/// JSON 数组，一个 10MB 的字体会膨胀成 30MB+ 文本，慢到不可用。
#[tauri::command]
async fn read_font_bytes(path: String) -> R<tauri::ipc::Response> {
    // 同步命令跑在主线程，读一个 10–40MB 的字体会把窗口冻住几百毫秒
    tauri::async_runtime::spawn_blocking(move || read_font_bytes_blocking(&path))
        .await
        .map_err(|err| format!("读取字体线程异常：{err}"))?
}

fn read_font_bytes_blocking(path: &str) -> R<tauri::ipc::Response> {
    const MAX_BYTES: u64 = 40 * 1024 * 1024;

    let p = std::path::Path::new(path);
    let ext = p
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if !matches!(ext.as_str(), "ttf" | "otf" | "ttc" | "woff" | "woff2") {
        return Err("不是支持的字体格式（ttf / otf / ttc / woff / woff2）".into());
    }

    let meta = std::fs::metadata(p).map_err(|err| format!("读取字体失败：{err}"))?;
    if !meta.is_file() {
        return Err("选中的不是文件".into());
    }
    if meta.len() > MAX_BYTES {
        return Err(format!(
            "字体文件 {:.1}MB，超过 40MB 上限",
            meta.len() as f64 / 1024.0 / 1024.0
        ));
    }

    let bytes = std::fs::read(p).map_err(|err| format!("读取字体失败：{err}"))?;
    Ok(tauri::ipc::Response::new(bytes))
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let default_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&default_dir)?;
            let data_dir = usable_data_dir(&default_dir);

            let mut lib = Library::open(&data_dir.join("library.db"))
                .map_err(|err| format!("初始化书架数据库失败：{err}"))?;

            // 启动时只做一次 stat，把丢失的文件标出来（不删记录）
            let _ = lib.refresh_missing();

            // EPUB 搜索缓存属于派生数据，跟着用户数据目录走
            novel_core::set_search_cache_dir(data_dir.join("search-cache"));

            // 窗口状态优先从 SQLite 读；旧插件的 json 会一次性迁进来
            let saved_window = load_window_state(&lib, &default_dir);

            app.manage(AppState {
                lib: Mutex::new(lib),
                data_dir,
                default_dir,
                import_cancel: AtomicBool::new(false),
            });

            if let Some(window) = app.get_webview_window("main") {
                if let Some(state) = saved_window {
                    apply_window_state(&window, &state);
                }
                let _ = window.show();

                let handle = app.handle().clone();
                let win = window.clone();
                window.on_window_event(move |event| {
                    if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                        persist_window_state(&handle, &win);
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_folders,
            create_folder,
            rename_folder,
            move_folder,
            reorder_folder,
            delete_folder,
            list_books,
            import_paths,
            import_folder,
            cancel_import,
            delete_book,
            set_book_folder,
            list_chapters,
            read_chapter,
            search,
            save_progress,
            get_progress,
            refresh_missing,
            get_setting,
            set_setting,
            data_dir,
            open_data_dir,
            has_library,
            change_data_dir,
            read_font_bytes,
            list_system_fonts,
            backfill_char_lens,
        ])
        .run(tauri::generate_context!())
        .expect("Tauri 启动失败");
}
