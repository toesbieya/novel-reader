/**
 * 应用设置（阅读偏好 + 书架排序）。
 *
 * 全部经 Rust 存进 library.db 的 setting 表；浏览器 localStorage 只用于
 * 首次升级时把旧偏好迁进来，迁移成功后立刻清掉旧键。
 */

import { DEFAULT_PREFS, type ReaderPrefs } from "../reader/prefs";
import { api } from "./ipc";
import { SORTS, type SortKey } from "./shelf-prefs";

const READER_KEY = "novel-reader.prefs.v1";
const SORT_KEY = "novel-reader.shelf.sort.v1";
const SORT_DESC_KEY = "novel-reader.shelf.sort-desc.v1";

let readerPrefs: ReaderPrefs = { ...DEFAULT_PREFS };
let shelfSort: SortKey = "default";
let shelfSortDesc = false;

/** 把 JSON 字符串解析成阅读偏好；坏数据返回 null */
function parseReader(raw: string | null): Partial<ReaderPrefs> | null {
  if (!raw) return null;
  try {
    const value = JSON.parse(raw) as unknown;
    return value && typeof value === "object" ? (value as Partial<ReaderPrefs>) : null;
  } catch {
    return null;
  }
}

/**
 * 启动时调用一次：从 SQLite 读设置；旧版本留在 localStorage 的偏好
 * 迁移到 SQLite 后再删掉旧键。失败时退回默认值，不阻塞应用启动。
 */
export async function initSettings(): Promise<void> {
  let dbReader: string | null = null;
  let dbSort: string | null = null;
  let dbDesc: string | null = null;
  let dbOk = true;
  try {
    [dbReader, dbSort, dbDesc] = await Promise.all([
      api.getSetting(READER_KEY),
      api.getSetting(SORT_KEY),
      api.getSetting(SORT_DESC_KEY),
    ]);
  } catch (err) {
    dbOk = false;
    console.warn("[settings] 读取设置失败，使用默认值", err);
  }

  let readerRaw = dbReader;
  let readerMigrated = dbReader != null;
  if (readerRaw == null) {
    const legacy = localStorage.getItem(READER_KEY);
    if (legacy != null) {
      readerRaw = legacy;
      if (dbOk) {
        try {
          await api.setSetting(READER_KEY, legacy);
          readerMigrated = true;
        } catch (err) {
          console.warn("[settings] 迁移阅读偏好失败", err);
        }
      }
    }
  }
  const parsed = parseReader(readerRaw);
  readerPrefs = { ...DEFAULT_PREFS, ...(parsed ?? {}) };

  let sortRaw = dbSort;
  let sortMigrated = dbSort != null;
  if (sortRaw == null) {
    const legacy = localStorage.getItem(SORT_KEY);
    if (legacy != null) {
      sortRaw = legacy;
      if (dbOk) {
        try {
          await api.setSetting(SORT_KEY, legacy);
          sortMigrated = true;
        } catch (err) {
          console.warn("[settings] 迁移书架排序失败", err);
        }
      }
    }
  }
  shelfSort = SORTS.some((s) => s.key === sortRaw) ? (sortRaw as SortKey) : "default";
  shelfSortDesc = dbDesc === "1" || dbDesc === "true";

  if (dbOk) {
    try {
      if (readerMigrated) localStorage.removeItem(READER_KEY);
      if (sortMigrated) localStorage.removeItem(SORT_KEY);
    } catch {
      // localStorage 不可用时忽略
    }
  }
}

/** 当前阅读偏好（可变；改完调用 saveReaderPrefs） */
export function getReaderPrefs(): ReaderPrefs {
  return readerPrefs;
}

let readerSaveTimer = 0;

/** 保存阅读偏好；拖动滑杆会高频调用，合并成一次 SQLite 写入 */
export function saveReaderPrefs(prefs: ReaderPrefs): void {
  readerPrefs = prefs;
  window.clearTimeout(readerSaveTimer);
  readerSaveTimer = window.setTimeout(flushReaderPrefs, 150);
}

/** 立刻落盘（页面关闭时调用，避免防抖窗口里的改动丢失） */
export function flushReaderPrefs(): void {
  window.clearTimeout(readerSaveTimer);
  void api.setSetting(READER_KEY, JSON.stringify(readerPrefs)).catch((err) => {
    console.warn("[settings] 保存阅读偏好失败", err);
  });
}

window.addEventListener("pagehide", flushReaderPrefs);

/** 当前书架排序 */
export function getShelfSort(): SortKey {
  return shelfSort;
}

/** 保存书架排序（低频操作，直接写） */
export function saveShelfSort(sort: SortKey): void {
  shelfSort = sort;
  void api.setSetting(SORT_KEY, sort).catch((err) => {
    console.warn("[settings] 保存书架排序失败", err);
  });
}

/** 排序是否倒序（相对该排序的自然方向） */
export function getShelfSortDesc(): boolean {
  return shelfSortDesc;
}

export function saveShelfSortDesc(desc: boolean): void {
  shelfSortDesc = desc;
  void api.setSetting(SORT_DESC_KEY, desc ? "1" : "0").catch((err) => {
    console.warn("[settings] 保存书架排序方向失败", err);
  });
}
