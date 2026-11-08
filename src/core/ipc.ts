/**
 * IPC 契约层 —— 前端与 novel-core 之间唯一的接缝。
 *
 * 两个通道：
 *  1. 在 Tauri 里 → invoke 到 Rust command
 *  2. 在浏览器里 → 走 mock，方便纯前端调 UI（pnpm dev 直接看）
 *
 * 所有字段名与 Rust 侧的 `#[serde(rename_all = "camelCase")]` 一一对应。
 * 调整结构只改这一处。
 */

import { invoke } from "@tauri-apps/api/core";

export type BookFormat = "txt" | "epub";

export interface BookSummary {
  id: number;
  folderId: number | null;
  title: string;
  author: string | null;
  format: BookFormat;
  encoding: string;
  chapterCount: number;
  /** 全书字符数（各章 char_len 之和）；老库未回填时是 0 */
  totalChars: number;
  filePath: string;
  fileSize: number;
  coverPath: string | null;
  /** 0..1 */
  progress: number;
  /** 上次阅读时间（unix 秒）；从没读过是 null */
  lastReadAt: number | null;
  /** 停在第几章（0 基）；从没读过是 null。和 progress 不是一回事 */
  lastChapter: number | null;
  /** 入库时间（unix 秒），「最近添加」排序与活动时间回退用 */
  addedAt: number;
  /** 原文件被移动或删除：书架项仍在，但打不开 */
  missing: boolean;
}

export interface FolderNode {
  id: number;
  parentId: number | null;
  name: string;
  sort: number;
}

export interface ChapterMeta {
  index: number;
  title: string;
}

export interface ChapterContent {
  index: number;
  title: string;
  text: string;
}

export interface SearchHit {
  chapterIndex: number;
  chapterTitle: string;
  /** 章内「字符」偏移，不是字节 */
  charOffset: number;
  snippet: string;
}

export interface SearchResult {
  hits: SearchHit[];
  /** 命中总数，可能远大于 hits.length */
  total: number;
  elapsedMs: number;
}

export interface Progress {
  chapterIndex: number;
  charOffset: number;
  updatedAt: number;
}

export interface ImportOutcome {
  id: number;
  title: string;
  chapterCount: number;
  encoding: string;
  /** 该文件已在书架里，没有重复导入 */
  alreadyExists: boolean;
}

export interface ImportReport {
  imported: ImportOutcome[];
  /** 逐个失败原因，不因一个坏文件中断整批导入 */
  failed: string[];
  /** 用户取消：剩余文件没处理，已完成的那几本仍然导入成功 */
  cancelled: boolean;
  /** 「导入文件夹」新建的分组目录数；普通文件导入是 0 */
  foldersCreated: number;
}

export interface DeleteOutcome {
  foldersRemoved: number;
  booksRemoved: number;
}

export const isTauri = (): boolean =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) return mock(cmd, args);
  return invoke<T>(cmd, args);
}

export const api = {
  // 目录
  listFolders: () => call<FolderNode[]>("list_folders"),
  createFolder: (parentId: number | null, name: string) =>
    call<number>("create_folder", { parentId, name }),
  renameFolder: (id: number, name: string) => call<void>("rename_folder", { id, name }),
  moveFolder: (id: number, parentId: number | null) => call<void>("move_folder", { id, parentId }),
  reorderFolder: (id: number, targetId: number, after: boolean) =>
    call<void>("reorder_folder", { id, targetId, after }),
  deleteFolder: (id: number) => call<DeleteOutcome>("delete_folder", { id }),

  // 书籍
  listBooks: () => call<BookSummary[]>("list_books"),
  importPaths: (paths: string[], folderId: number | null) =>
    call<ImportReport>("import_paths", { paths, folderId }),
  /** 递归扫描文件夹里的 txt / epub，按子文件夹建分组目录（同级同名合并） */
  importFolder: (path: string, folderId: number | null) =>
    call<ImportReport>("import_folder", { path, folderId }),
  /** 停止当前导入批次；当前这本会做完，剩下的跳过 */
  cancelImport: () => call<void>("cancel_import"),
  deleteBook: (id: number) => call<void>("delete_book", { id }),
  setBookFolder: (bookId: number, folderId: number | null) =>
    call<void>("set_book_folder", { bookId, folderId }),

  // 阅读
  listChapters: (bookId: number) => call<ChapterMeta[]>("list_chapters", { bookId }),
  readChapter: (bookId: number, index: number) =>
    call<ChapterContent>("read_chapter", { bookId, index }),
  search: (bookId: number, query: string, limit = 200, offset = 0) =>
    call<SearchResult>("search", { bookId, query, limit, offset }),
  saveProgress: (bookId: number, chapterIndex: number, charOffset: number) =>
    call<void>("save_progress", { bookId, chapterIndex, charOffset }),
  getProgress: (bookId: number) => call<Progress | null>("get_progress", { bookId }),

  // 设置 / 数据目录
  /** 读一条应用设置（阅读偏好、书架排序、窗口状态等），都存 library.db */
  getSetting: (key: string) => call<string | null>("get_setting", { key }),
  setSetting: (key: string, value: string) => call<void>("set_setting", { key, value }),
  /** 当前用户数据目录 */
  dataDir: () => call<string>("data_dir"),
  /** 在资源管理器里打开当前数据目录 */
  openDataDir: () => call<void>("open_data_dir"),
  /** 目标目录里是否已有 library.db */
  hasLibrary: (dir: string) => call<boolean>("has_library", { path: dir }),
  /** 迁移/切换数据目录并重启；成功时不会返回 */
  changeDataDir: (newDir: string, useExisting: boolean) =>
    call<void>("change_data_dir", { newDir, useExisting }),

  // 其它
  refreshMissing: () => call<number>("refresh_missing"),
  /** 字体文件原始字节，Tauri 侧走二进制通道；浏览器模式不支持 */
  readFontBytes: (path: string) => call<ArrayBuffer>("read_font_bytes", { path }),
  /** 系统已安装的字体族名，供阅读设置里直接选用 */
  listSystemFonts: () => call<string[]>("list_system_fonts"),
  /** 给老库补章节字数（进度按字数加权用），返回补了几本 */
  backfillCharLens: () => call<number>("backfill_char_lens"),
};

/** 打开系统文件选择器，只允许 txt / epub。浏览器模式下返回空数组。 */
export async function pickEbookFiles(): Promise<string[]> {
  if (!isTauri()) return [];
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    multiple: true,
    title: "选择 txt / epub",
    filters: [{ name: "电子书", extensions: ["txt", "epub"] }],
  });
  if (!picked) return [];
  return Array.isArray(picked) ? picked : [picked];
}

/** 打开系统文件选择器，只挑字体文件。浏览器模式下返回 null。 */
export async function pickFontFile(): Promise<string | null> {
  if (!isTauri()) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    multiple: false,
    title: "选择字体文件",
    filters: [{ name: "字体", extensions: ["ttf", "otf", "ttc", "woff", "woff2"] }],
  });
  if (!picked) return null;
  return Array.isArray(picked) ? (picked[0] ?? null) : picked;
}

/** 打开系统文件夹选择器（迁移数据目录用）。浏览器模式下返回 null。 */
/** 打开系统目录选择器。默认给数据目录用，导入文件夹时传自己的标题。 */
export async function pickDirectory(title = "选择数据目录"): Promise<string | null> {
  if (!isTauri()) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    directory: true,
    multiple: false,
    title,
  });
  return typeof picked === "string" ? picked : null;
}

export interface ImportProgress {
  done: number;
  total: number;
  /** 刚处理完的那本书（失败时是文件名） */
  title: string;
  ok: boolean;
}

/**
 * 订阅导入进度。导入跑在后端线程里，靠事件把"第几本"推回来，
 * 否则用户在 66 本导入期间只能看着一个不会动的「导入中…」。
 */
export async function onImportProgress(
  handler: (p: ImportProgress) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<ImportProgress>("import-progress", (e) => handler(e.payload));
}

/** 原文件在应用外被改动、目录已重建的通知 */
export interface BookReindexed {
  /** 重建过索引的书 */
  bookId: number;
  /** 书名，直接用于提示文案 */
  title: string;
}

/**
 * 订阅"文件变了、目录已重建"。
 * 事件在阅读页读章节 / 搜索时才会产生，所以监听放在 app 层而不是书架里。
 */
export async function onBookReindexed(
  handler: (p: BookReindexed) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<BookReindexed>("book-reindexed", (e) => handler(e.payload));
}

export type DragPhase = "over" | "leave" | "drop";

/**
 * 监听拖入窗口的文件。
 * 返回取消订阅函数——书架视图会被反复重建，不取消就会累积监听。
 */
export async function onEbookDrag(
  handler: (phase: DragPhase, paths: string[]) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { getCurrentWebview } = await import("@tauri-apps/api/webview");
  return getCurrentWebview().onDragDropEvent((event) => {
    const p = event.payload;
    if (p.type === "drop") handler("drop", p.paths);
    else if (p.type === "leave") handler("leave", []);
    else handler("over", []);
  });
}

/** 只保留能导入的扩展名，别把整个文件夹拖进来时产生一堆报错 */
export function filterEbooks(paths: string[]): string[] {
  return paths.filter((p) => /\.(txt|epub)$/i.test(p));
}

// ---------------------------------------------------------------- mock

const MOCK_FOLDERS: FolderNode[] = [
  { id: 1, parentId: null, name: "玄幻", sort: 0 },
  { id: 2, parentId: 1, name: "仙侠", sort: 0 },
  { id: 3, parentId: 2, name: "古典仙侠", sort: 0 },
  { id: 4, parentId: null, name: "悬疑", sort: 1 },
  { id: 5, parentId: null, name: "待读", sort: 2 },
];

/** 相对当前时间造数据，浏览器 preview 里「3 天前」才是真的三天前 */
const MOCK_NOW = Math.floor(Date.now() / 1000);
const DAY = 86_400;

function book(
  id: number,
  title: string,
  author: string,
  chapterCount: number,
  progress: number,
  lastReadAt: number | null,
  folderId: number | null,
): BookSummary {
  return {
    id,
    folderId,
    title,
    author,
    format: "txt",
    encoding: "GB18030",
    chapterCount,
    // 真实库按 chapter.char_len 求和；mock 按每章约 2800 字估
    totalChars: chapterCount * 2800,
    filePath: `D:\\novels\\${title}.txt`,
    fileSize: chapterCount * 7200,
    coverPath: null,
    progress,
    lastReadAt,
    // 真实后端在 progress 之外还存着 chapter_idx；mock 按比例反推一个够用
    lastChapter:
      progress > 0 ? Math.min(chapterCount - 1, Math.round(progress * chapterCount)) : null,
    addedAt: MOCK_NOW - (60 - id) * 86400,
    missing: false,
  };
}

const MOCK_BOOKS: BookSummary[] = [
  book(1, "剑来", "烽火戏诸侯", 1268, 0.62, MOCK_NOW - 2 * DAY, 3),
  book(2, "诡秘之主", "爱潜水的乌贼", 1431, 1, MOCK_NOW - 5 * DAY, 1),
  book(3, "凡人修仙传", "忘语", 2456, 0.18, MOCK_NOW - 9 * DAY, 3),
  book(4, "大奉打更人", "卖报小郎君", 986, 0.94, MOCK_NOW - 14 * DAY, 1),
  book(5, "赤心巡天", "情何以甚", 1102, 0.07, MOCK_NOW - 22 * DAY, 4),
  book(6, "夜的命名术", "会说话的肘子", 812, 0.35, MOCK_NOW - 40 * DAY, 4),
  book(7, "长夜余火", "爱潜水的乌贼", 734, 0.51, MOCK_NOW - 60 * DAY, 5),
  book(8, "宿命之环", "爱潜水的乌贼", 623, 0.0, null, 5),
];

const MAX_FOLDER_DEPTH = 5;

/**
 * 下面这几个函数是 store.rs 里同名逻辑的内存版。
 * 规则必须对齐，否则浏览器 preview 会放过后端真正会拒绝的操作，
 * 到了 Tauri 里才炸——那就白调了。
 */
function folderDepth(id: number): number {
  let depth = 1;
  let cur = MOCK_FOLDERS.find((f) => f.id === id);
  for (let guard = 0; cur && cur.parentId !== null && guard <= MAX_FOLDER_DEPTH; guard++) {
    const parentId: number = cur.parentId;
    cur = MOCK_FOLDERS.find((f) => f.id === parentId);
    if (cur) depth += 1;
  }
  return depth;
}

function folderSubtree(id: number): number[] {
  const ids = [id];
  for (let i = 0; i < ids.length; i++) {
    for (const f of MOCK_FOLDERS) {
      if (f.parentId === ids[i]) ids.push(f.id);
    }
  }
  return ids;
}

function isDescendant(candidate: number, ancestor: number): boolean {
  let cur = MOCK_FOLDERS.find((f) => f.id === candidate);
  for (let guard = 0; cur && cur.parentId !== null && guard <= MAX_FOLDER_DEPTH; guard++) {
    if (cur.parentId === ancestor) return true;
    const parentId: number = cur.parentId;
    cur = MOCK_FOLDERS.find((f) => f.id === parentId);
  }
  return false;
}

function checkFolderName(raw: unknown): string {
  const t = String(raw ?? "").trim();
  if (!t) throw new Error("目录名不能为空");
  if (t.length > 60) throw new Error("目录名太长");
  if (t.includes("/") || t.includes("\\")) throw new Error("目录名不能包含 / 或 \\");
  return t;
}

function mock<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  switch (cmd) {
    // 返回浅拷贝：调用方会用 signal.set(arr) 判断引用是否变化来触发重渲染，
    // 直接给 MOCK_* 会让 Object.is 恒等，操作完界面纹丝不动。
    case "list_books":
      return Promise.resolve(MOCK_BOOKS.map((b) => ({ ...b })) as unknown as T);
    case "list_folders":
      // 与 SQL 侧一致：先按父目录、再按 sort、最后按 id
      return Promise.resolve(
        [...MOCK_FOLDERS]
          .sort(
            (a, b) =>
              (a.parentId ?? -1) - (b.parentId ?? -1) || a.sort - b.sort || a.id - b.id,
          )
          .map((f) => ({ ...f })) as unknown as T,
      );
    case "list_chapters":
      return Promise.resolve(mockChapters(Number(args?.bookId ?? 1)) as unknown as T);
    case "read_chapter":
      return Promise.resolve(
        mockChapter(Number(args?.bookId ?? 1), Number(args?.index ?? 0)) as unknown as T,
      );
    case "get_progress":
      return Promise.resolve(null as unknown as T);
    case "save_progress":
      return Promise.resolve(undefined as unknown as T);
    case "search":
      return Promise.resolve(
        mockSearch(
          String(args?.query ?? ""),
          Number(args?.limit ?? 200),
          Number(args?.offset ?? 0),
        ) as unknown as T,
      );

    case "get_setting": {
      const key = String(args?.key ?? "");
      return Promise.resolve(localStorage.getItem(`mock-setting:${key}`) as unknown as T);
    }
    case "set_setting": {
      const key = String(args?.key ?? "");
      localStorage.setItem(`mock-setting:${key}`, String(args?.value ?? ""));
      return Promise.resolve(undefined as unknown as T);
    }
    case "data_dir":
      return Promise.resolve("浏览器 preview 没有数据目录" as unknown as T);
    case "open_data_dir":
      return Promise.reject(new Error("浏览器 preview 没有数据目录"));
    case "has_library":
      return Promise.resolve(false as unknown as T);
    case "change_data_dir":
      return Promise.reject(new Error("浏览器 preview 不支持修改数据目录"));

    case "read_font_bytes":
      return Promise.reject(new Error("浏览器 preview 模式不支持导入本地字体，请在 Tauri 里试"));

    case "list_system_fonts":
      // 浏览器 preview 用常见 Windows 中文字体当示例
      return Promise.resolve([
        "Microsoft YaHei",
        "Microsoft YaHei UI",
        "SimSun",
        "SimHei",
        "KaiTi",
        "FangSong",
        "DengXian",
      ] as unknown as T);

    case "backfill_char_lens":
      // mock 里的进度是写死的，没有需要补的数据
      return Promise.resolve(0 as unknown as T);

    case "create_folder": {
      const parentId = (args?.parentId ?? null) as number | null;
      const name = checkFolderName(args?.name);
      if (parentId !== null && !MOCK_FOLDERS.some((f) => f.id === parentId)) {
        throw new Error(`目录不存在：${parentId}`);
      }
      const depth = parentId === null ? 0 : folderDepth(parentId);
      if (depth + 1 > MAX_FOLDER_DEPTH) throw new Error(`目录最多 ${MAX_FOLDER_DEPTH} 级`);
      if (MOCK_FOLDERS.some((f) => f.parentId === parentId && f.name === name)) {
        throw new Error(`同级已有同名目录：${name}`);
      }
      const id = MOCK_FOLDERS.reduce((m, f) => Math.max(m, f.id), 0) + 1;
      const sort = MOCK_FOLDERS.filter((f) => f.parentId === parentId).length;
      MOCK_FOLDERS.push({ id, parentId, name, sort });
      return Promise.resolve(id as unknown as T);
    }

    case "rename_folder": {
      const id = Number(args?.id);
      const name = checkFolderName(args?.name);
      const target = MOCK_FOLDERS.find((f) => f.id === id);
      if (!target) throw new Error(`目录不存在：${id}`);
      const dup = MOCK_FOLDERS.find(
        (f) => f.id !== id && f.parentId === target.parentId && f.name === name,
      );
      if (dup) throw new Error(`同级已有同名目录：${name}`);
      target.name = name;
      return Promise.resolve(undefined as unknown as T);
    }

    case "move_folder": {
      const id = Number(args?.id);
      const parentId = (args?.parentId ?? null) as number | null;
      if (!MOCK_FOLDERS.some((f) => f.id === id)) throw new Error(`目录不存在：${id}`);
      if (parentId === id) throw new Error("不能把目录挪到自己下面");
      if (parentId !== null && isDescendant(parentId, id)) {
        throw new Error("不能把目录挪到自己的子目录下");
      }
      const selfDepth = folderDepth(id);
      const height = folderSubtree(id).reduce(
        (m, fid) => Math.max(m, folderDepth(fid) - selfDepth + 1),
        1,
      );
      const base = parentId === null ? 0 : folderDepth(parentId);
      if (base + height > MAX_FOLDER_DEPTH) {
        throw new Error(`移动后超过 ${MAX_FOLDER_DEPTH} 级上限`);
      }
      MOCK_FOLDERS.find((f) => f.id === id)!.parentId = parentId;
      return Promise.resolve(undefined as unknown as T);
    }

    case "delete_folder": {
      const id = Number(args?.id);
      if (!MOCK_FOLDERS.some((f) => f.id === id)) throw new Error(`目录不存在：${id}`);
      const ids = folderSubtree(id);
      const kill = new Set(ids);
      let booksRemoved = 0;
      for (let i = MOCK_BOOKS.length - 1; i >= 0; i--) {
        const fid = MOCK_BOOKS[i].folderId;
        if (fid !== null && kill.has(fid)) {
          MOCK_BOOKS.splice(i, 1);
          booksRemoved += 1;
        }
      }
      for (let i = MOCK_FOLDERS.length - 1; i >= 0; i--) {
        if (kill.has(MOCK_FOLDERS[i].id)) MOCK_FOLDERS.splice(i, 1);
      }
      return Promise.resolve({ foldersRemoved: ids.length, booksRemoved } as unknown as T);
    }

    case "delete_book": {
      const id = Number(args?.id);
      const at = MOCK_BOOKS.findIndex((b) => b.id === id);
      if (at < 0) throw new Error(`书籍不存在：${id}`);
      MOCK_BOOKS.splice(at, 1);
      return Promise.resolve(undefined as unknown as T);
    }

    // 浏览器里没有真实导入流程（文件选择器返回空），置位也无害
    case "cancel_import":
      return Promise.resolve(undefined as unknown as T);

    case "set_book_folder": {
      const bookId = Number(args?.bookId);
      const folderId = (args?.folderId ?? null) as number | null;
      const target = MOCK_BOOKS.find((b) => b.id === bookId);
      if (!target) throw new Error(`书籍不存在：${bookId}`);
      target.folderId = folderId;
      return Promise.resolve(undefined as unknown as T);
    }

    case "reorder_folder": {
      const id = Number(args?.id);
      const targetId = Number(args?.targetId);
      const after = Boolean(args?.after);
      if (id === targetId) return Promise.resolve(undefined as unknown as T);

      const self = MOCK_FOLDERS.find((f) => f.id === id);
      const target = MOCK_FOLDERS.find((f) => f.id === targetId);
      if (!self) throw new Error(`目录不存在：${id}`);
      if (!target) throw new Error(`目录不存在：${targetId}`);
      if (self.parentId !== target.parentId) throw new Error("只能在同一级目录内排序");

      const siblings = MOCK_FOLDERS.filter((f) => f.parentId === self.parentId && f.id !== id).sort(
        (a, b) => a.sort - b.sort || a.id - b.id,
      );
      const at = siblings.findIndex((f) => f.id === targetId);
      if (at < 0) throw new Error("目标目录不在这棵子树里");
      siblings.splice(after ? at + 1 : at, 0, self);
      siblings.forEach((f, i) => {
        f.sort = i;
      });
      return Promise.resolve(undefined as unknown as T);
    }

    default:
      return Promise.reject(new Error(`浏览器 preview 模式未实现：${cmd}`));
  }
}

// ---------------------------------------------------------------- 阅读页 mock 语料

const TITLE_WORDS = [
  "风起", "夜行", "旧梦", "归途", "山雨", "断剑", "残阳", "远客", "孤舟", "春深",
  "长歌", "暗涌", "落雪", "问心", "无名", "边城", "故人", "青锋", "月下", "尘缘",
];

const SENTENCES = [
  "山道上的雪积了半尺，踩下去发出细碎的响声，像是有人在极远处敲着一面很小的鼓。",
  "他把手拢进袖子里，站在檐下看了很久，看雨水顺着瓦当一滴一滴落进石槽。",
  "老人说这条路走的人少，你要习惯一个人走，走到最后连自己的脚步声都听不见。",
  "灯芯爆了一下，火光晃了晃，墙上的影子跟着歪了半边。",
  "那年的春天来得格外迟，桃花开了三日便被一场冷雨打落，铺了满地。",
  "她始终没有回头，只是把伞往肩上又挪了挪，好像那样就能把风挡住。",
  "剑在鞘中，鞘在腰间，腰间空空荡荡，他忽然不记得自己为什么要往这边走。",
  "远处传来钟声，一下，又一下，数到第十三下的时候，声音就散了。",
  "窗外的雨停了，屋檐还在滴水，滴答、滴答，像有人在数着什么。",
  "他伸手去推那扇门，门却自己开了，里面黑得看不见底，只有一股旧木头的气味涌出来。",
  "桌上有半盏冷茶，茶面上浮着一层薄薄的灰，显然已经放了很多天。",
  "街上没有人，只有一只黄狗蹲在墙角，看见他过来，懒懒地抬了下眼皮。",
  "他说这话的时候语气很平，像是在讲一件和自己毫不相干的事。",
  "风从谷底卷上来，带着湿冷的土腥气，把衣角吹得猎猎作响。",
  "那一瞬间他忽然明白，这件事从一开始就没有回头路可以走。",
  "天色一点点暗下去，远处的山影压过来，像一堵慢慢逼近的墙。",
  "她笑了一下，那笑意很淡，散在眼睛里，转瞬就没了。",
  "柴火在灶膛里噼啪作响，映得半间屋子都是暖的红色。",
  "队伍走得很慢，前面的人不时停下来，像是在等什么，又像是不敢往前。",
  "他把那张纸对折两次，塞进怀里，动作很轻，仿佛怕它碎了。",
  "河面结着薄冰，冰下隐约有水流的声音，细而持续。",
  "这一带的人都姓同一个姓，问起来，多半还能攀上些拐弯抹角的亲戚关系。",
  "夜很静，静得能听见自己心跳的声音，一下一下，撞在肋骨上。",
  "他知道自己该走了，可脚像是生了根，挪不动半分。",
];

function seeded(seed: number): () => number {
  let s = seed >>> 0 || 1;
  return () => {
    s ^= s << 13;
    s ^= s >>> 17;
    s ^= s << 5;
    s >>>= 0;
    return s / 0xffffffff;
  };
}

function mockChapterCount(bookId: number): number {
  const b = MOCK_BOOKS.find((x) => x.id === bookId);
  return Math.max(3, Math.min(b?.chapterCount ?? 40, 1400));
}

function mockChapterTitle(bookId: number, index: number): string {
  const word = TITLE_WORDS[(bookId * 7 + index) % TITLE_WORDS.length];
  return `第${index + 1}章 ${word}`;
}

function mockChapters(bookId: number): ChapterMeta[] {
  const n = mockChapterCount(bookId);
  return Array.from({ length: n }, (_, i) => ({
    index: i,
    title: mockChapterTitle(bookId, i),
  }));
}

function mockChapter(bookId: number, index: number): ChapterContent {
  const rnd = seeded(bookId * 100003 + index * 7919 + 17);
  const lines: string[] = [mockChapterTitle(bookId, index), ""];
  const paraCount = 26 + Math.floor(rnd() * 14);

  for (let p = 0; p < paraCount; p++) {
    const n = 2 + Math.floor(rnd() * 4);
    let para = "";
    for (let s = 0; s < n; s++) {
      para += SENTENCES[Math.floor(rnd() * SENTENCES.length)];
    }
    lines.push(para, "");
  }

  return { index, title: mockChapterTitle(bookId, index), text: lines.join("\n") };
}

function mockSearch(query: string, limit = 200, offset = 0): SearchResult {
  const q = query.trim();
  if (!q) return { hits: [], total: 0, elapsedMs: 0 };

  // 造出比一页更多的命中，浏览器 preview 里才能走查「加载更多」；
  // 和真实引擎一致：total 是全书总数，hits 只是 [offset, offset+limit) 这一段。
  const all: SearchHit[] = [];
  const CAP = 480;
  for (let i = 0; i < 12 && all.length < CAP; i++) {
    const chapter = mockChapter(1, i);
    // 同一章里的每一处都算一条：真实的 search.rs 也是全扫，
    // 每章只给一条会让"点第 3 条"这种场景根本没法在浏览器里复现。
    let from = 0;
    while (all.length < CAP) {
      const at = chapter.text.indexOf(q, from);
      if (at < 0) break;

      // 和 search.rs 保持同一契约：关键字前 10 字、后 30 字，稳稳落在结果行第一行
      const start = Math.max(0, at - 10);
      const end = Math.min(chapter.text.length, at + q.length + 30);
      const head = start > 0 ? "…" : "";
      const tail = end < chapter.text.length ? "…" : "";
      all.push({
        chapterIndex: i,
        chapterTitle: chapter.title,
        charOffset: at,
        snippet: `${head}${chapter.text.slice(start, end).replace(/\s+/g, " ")}${tail}`,
      });
      from = at + q.length;
    }
  }

  return { hits: all.slice(offset, offset + limit), total: all.length, elapsedMs: 0.4 };
}
