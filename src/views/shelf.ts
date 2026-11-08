import { appendAll, clear, h } from "../core/dom";
import {
  chooseFolder,
  confirmAction,
  promptText,
  showDataDirDialog,
  type ChooseItem,
} from "../core/dialog";
import { startDrag, type DragTarget } from "../core/drag";
import { closeContextMenu, showContextMenu, type MenuItem } from "../core/menu";
import { effect, signal } from "../core/reactive";
import {
  getShelfSort,
  getShelfSortDesc,
  saveShelfSort,
  saveShelfSortDesc,
} from "../core/settings";
import { SORTS, type SortKey } from "../core/shelf-prefs";
import {
  api,
  filterEbooks,
  isTauri,
  onEbookDrag,
  onImportProgress,
  pickDirectory,
  pickEbookFiles,
  type BookSummary,
  type FolderNode,
  type ImportReport,
} from "../core/ipc";

type Filter = "all" | "recent" | "unread";

const FILTERS: { key: Filter; label: string }[] = [
  { key: "all", label: "全部书籍" },
  { key: "recent", label: "最近阅读" },
  { key: "unread", label: "未读" },
];

/** 与 crates/novel-core/src/store.rs 的 MAX_FOLDER_DEPTH 保持一致 */
const MAX_FOLDER_DEPTH = 5;

/**
 * 书架的浏览态。**必须活在模块里**：从阅读页返回时 `app.ts` 会整个重建
 * `ShelfView`，视图内的 signal 全部重新初始化——用局部状态的话，在「仙侠」
 * 里点开一本书再回来，就会掉回「全部书籍」顶部，搜索词也没了。
 */
const session = {
  filter: "all" as Filter,
  keyword: "",
  folderId: null as number | null,
  scrollTop: 0,
  /** 折叠的目录 id。只在本次运行内记忆，不进 library.db */
  collapsed: new Set<number>(),
};

/** 书架会被反复重建（从阅读页返回），拖拽监听必须跨实例回收 */
let dropUnlisten: (() => void) | null = null;
/** 导入进度事件同理，不回收会越堆越多 */
let progressUnlisten: (() => void) | null = null;
/** Ctrl+F 聚焦书架搜索框；进阅读页时移除，避免残留监听 */
let searchKeyUnlisten: (() => void) | null = null;
let instanceSeq = 0;
let toastTimer = 0;

export function ShelfView(onOpen: (book: BookSummary) => void): HTMLElement {
  // 每次从阅读页返回都会重建书架，残留的右键菜单必须先收掉，
  // 否则它会挂在一个已经被替换掉的 DOM 上，点哪儿都关不掉。
  closeContextMenu();
  progressUnlisten?.();
  progressUnlisten = null;
  searchKeyUnlisten?.();
  searchKeyUnlisten = null;
  if (dropUnlisten) {
    dropUnlisten();
    dropUnlisten = null;
  }
  const token = ++instanceSeq;

  const books = signal<BookSummary[]>([]);
  const folders = signal<FolderNode[]>([]);
  // 初值来自上一次会话，刷新一次就能回到原处
  const filter = signal<Filter>(session.filter);
  const keyword = signal(session.keyword);
  const sort = signal<SortKey>(getShelfSort());
  const sortDesc = signal(getShelfSortDesc());
  const currentFolder = signal<number | null>(session.folderId);
  const busy = signal(false);
  /** 导入进度；null = 没有在导入 */
  const importing = signal<{ done: number; total: number; title: string } | null>(null);
  const cancelling = signal(false);
  /** 刚导入的书 id，渲染成高亮并在 2 秒后清掉 */
  const fresh = signal<ReadonlySet<number>>(new Set());
  /** 折叠状态不在 signal 里，改完敲一下让它重跑 */
  const treeTick = signal(0);

  const titleEl = h("h1", { class: "title" });
  const scopeEl = h("span", { class: "scope-chip" }, "全库");
  const countEl = h("span", { class: "count" });
  const grid = h("div", {
    class: "grid",
    onScroll: () => {
      session.scrollTop = grid.scrollTop;
    },
  });
  const continueHost = h("div", { class: "continue-host" });
  const navList = h("nav", { class: "nav" });
  const treeList = h("div", {
    class: "tree",
    // 空白处右键 = 在根目录下新建（子项的 contextmenu 里已 stopPropagation）
    onContextMenu: (e: Event) => {
      if (e.target !== treeList) return;
      e.preventDefault();
      const me = e as MouseEvent;
      showContextMenu(me.clientX, me.clientY, [
        { label: "新建目录", onClick: () => void newFolderDialog(null) },
      ]);
    },
  });

  const toast = h("div", { class: "toast" }, "");
  const dropMask = h(
    "div",
    { class: "drop-mask" },
    h("div", { class: "drop-card" }, "松开即可导入 txt / epub"),
  );

  const importBtn = h(
    "button",
    { class: "import-btn", type: "button", onClick: () => void pickAndImport() },
    "导入 txt / epub",
  );
  const importFolderBtn = h(
    "button",
    {
      class: "import-folder-btn",
      type: "button",
      title: "递归扫描文件夹里的 txt / epub，按子文件夹建分组目录（同级同名合并）",
      onClick: () => void pickAndImportFolder(),
    },
    "导入文件夹",
  );
  const importFill = h("i");
  const importTrack = h("div", { class: "import-track" }, importFill);
  const importLabel = h("span", { class: "import-label" }, "");
  const importCancel = h(
    "button",
    { class: "import-cancel", type: "button", onClick: () => void cancelImport() },
    "取消",
  );
  const importPanel = h(
    "div",
    { class: "import-panel" },
    importTrack,
    h("div", { class: "import-line" }, importLabel, importCancel),
  );

  // ------------------------------------------------------------ 渲染

  /** 进阅读页前回收本视图的全部全局监听，阅读页会自己接管 Ctrl+F。
   * 少了 drop / progress 两条，进阅读页后拖 txt 进窗还会调旧闭包的 importFiles。 */
  function openAndCleanup(book: BookSummary): void {
    searchKeyUnlisten?.();
    searchKeyUnlisten = null;
    progressUnlisten?.();
    progressUnlisten = null;
    if (dropUnlisten) {
      dropUnlisten();
      dropUnlisten = null;
    }
    onOpen(book);
  }

  /** 文件失效的卡片不能再是「点了没反应」：把原因和原路径都说清楚 */
  function handleOpen(book: BookSummary): void {
    if (!book.missing) {
      openAndCleanup(book);
      return;
    }
    showToast(`「${book.title}」的文件已失效：${book.filePath}`, "err");
  }

  effect(() => {
    const fid = currentFolder();
    const kw = keyword().trim();
    const searching = kw.length > 0;

    titleEl.textContent = searching
      ? `搜索：${kw}`
      : fid === null
        ? (FILTERS.find((f) => f.key === filter())?.label ?? "全部书籍")
        : (folders().find((f) => f.id === fid)?.name ?? "未知目录");
    scopeEl.classList.toggle("is-on", searching);

    const list = visible({
      all: books(),
      filter: filter(),
      keyword: keyword(),
      // 进目录看到的是整棵子树：目录树上的计数也是含子目录的，
      // 两边不一致会让人以为「显示 4 本，点进去只有 2 本」是坏了
      folderScope: fid === null ? null : new Set(subtreeOf(folders(), fid)),
      sort: sort(),
      desc: sortDesc(),
    });

    const paths = folderPaths(folders());
    const marked = fresh();
    clear(grid);
    if (list.length === 0) {
      // 搜索无结果 / 筛选为空 / 空目录各说各的话，别一律让用户「去导入」
      const reason: EmptyReason = searching
        ? "search"
        : filter() !== "all" || fid !== null
          ? "filter"
          : "empty";
      appendAll(grid, [
        EmptyState(reason, () => {
          keyword.set("");
          filter.set("all");
          currentFolder.set(null);
        }),
      ]);
    } else {
      appendAll(
        grid,
        list.map((b) =>
          BookCard(b, {
            // 搜索是跨目录的，卡片必须自己说明「这本书在哪」，
            // 否则连载分卷 / 同名书根本分不清是哪一本
            path: searching
              ? b.folderId === null
                ? "未分类"
                : (paths.get(b.folderId) ?? "未分类")
              : null,
            fresh: marked.has(b.id),
            onOpen: handleOpen,
            onMenu: (book, x, y) => bookMenu(book, x, y),
            onDrag: (book, e) => dragBook(e, book),
          }),
        ),
      );
    }

    const total = books().length;
    countEl.textContent = list.length === total ? `${total} 本` : `${list.length} / ${total} 本`;
  });

  // 继续阅读：只在「全部书籍 + 无搜索」的首页出现，进目录或搜索时让位给结果列表
  effect(() => {
    const list = books();
    const show = keyword().trim() === "" && currentFolder() === null && filter() === "all";
    clear(continueHost);
    const book = show ? continueBook(list) : null;
    if (book) continueHost.appendChild(ContinueCard(book, () => handleOpen(book)));
  });

  effect(() => {
    const active = filter();
    const inRoot = currentFolder() === null;
    const all = books();
    const counts: Record<Filter, number> = {
      all: all.length,
      recent: all.filter((b) => b.lastReadAt !== null).length,
      unread: all.filter((b) => b.progress === 0).length,
    };

    clear(navList);
    for (const item of FILTERS) {
      navList.appendChild(
        h(
          "button",
          {
            class: `nav-item${active === item.key && inRoot ? " is-active" : ""}`,
            title: item.key === "all" ? "把书拖到这里 = 移出目录" : undefined,
            dataset: item.key === "all" ? { dropRoot: "1" } : undefined,
            onClick: () => {
              filter.set(item.key);
              currentFolder.set(null);
            },
          },
          h("span", { class: "nav-label" }, item.label),
          h("span", { class: "nav-count" }, String(counts[item.key])),
        ),
      );
    }
  });

  effect(() => {
    treeTick();
    const active = currentFolder();
    const counts = folderCounts(folders(), books());

    clear(treeList);
    for (const { node, depth, hasChildren } of flatten(folders(), session.collapsed)) {
      const atLimit = depth + 1 >= MAX_FOLDER_DEPTH;
      const folded = session.collapsed.has(node.id);
      const count = counts.get(node.id) ?? 0;

      treeList.appendChild(
        h(
          "div",
          {
            class: `tree-row${active === node.id ? " is-active" : ""}`,
            style: { paddingLeft: `${6 + depth * 13}px` },
            title: "右键管理目录，拖动可排序",
            role: "button",
            tabIndex: 0,
            dataset: { folderId: String(node.id) },
            onPointerDown: (e: Event) => {
              // 点折叠箭头不是拖拽，也不切换目录
              if ((e.target as Element).closest(".tree-toggle")) return;
              dragFolder(e as PointerEvent, node);
            },
            onClick: (e: Event) => {
              if ((e.target as Element).closest(".tree-toggle")) return;
              filter.set("all");
              currentFolder.set(node.id);
            },
            onKeyDown: (e: Event) => {
              const ke = e as KeyboardEvent;
              if (ke.key !== "Enter" && ke.key !== " ") return;
              ke.preventDefault();
              filter.set("all");
              currentFolder.set(node.id);
            },
            onContextMenu: (e: Event) => {
              e.preventDefault();
              e.stopPropagation();
              const me = e as MouseEvent;
              showContextMenu(me.clientX, me.clientY, [
                {
                  label: "新建子目录",
                  disabled: atLimit,
                  hint: atLimit ? `已到 ${MAX_FOLDER_DEPTH} 级` : undefined,
                  onClick: () => void newFolderDialog(node.id),
                },
                { sep: true },
                { label: "重命名", onClick: () => void renameDialog(node) },
                { label: "移动到…", onClick: () => void moveFolderDialog(node) },
                { sep: true },
                { label: "删除目录", danger: true, onClick: () => void deleteFolderDialog(node) },
              ]);
            },
          },
          hasChildren
            ? h("button", {
                class: `tree-toggle${folded ? " is-folded" : ""}`,
                type: "button",
                title: folded ? "展开" : "折叠",
                "aria-label": folded ? `展开 ${node.name}` : `折叠 ${node.name}`,
                onClick: (e: Event) => {
                  e.stopPropagation();
                  toggleCollapse(node.id);
                },
              })
            : h("span", { class: "tree-toggle is-empty" }),
          h("span", { class: "tree-label" }, node.name),
          h(
            "span",
            { class: "tree-count", title: `含子目录共 ${count} 本` },
            String(count),
          ),
        ),
      );
    }
  });

  effect(() => {
    const b = busy();
    const p = importing();

    importBtn.disabled = b;
    importFolderBtn.disabled = b;
    importBtn.textContent = b ? "导入中…" : "导入 txt / epub";
    importPanel.classList.toggle("is-on", b);

    if (p) {
      const pct = p.total > 0 ? Math.round((p.done / p.total) * 100) : 0;
      importFill.style.width = `${pct}%`;
      importLabel.textContent = `${p.done} / ${p.total} · ${p.title}`;
      importLabel.title = p.title;
    } else {
      importFill.style.width = "0%";
      importLabel.textContent = "";
      importLabel.title = "";
    }

    importCancel.disabled = cancelling();
    importCancel.textContent = cancelling() ? "正在停止…" : "取消";
  });

  // 三个浏览态镜像到 session；重建视图时靠它回填
  effect(() => {
    session.filter = filter();
    session.keyword = keyword();
    session.folderId = currentFolder();
  });

  let restored = false;
  void refresh();

  // 升级前的书没有章节字数，进度只能按章节序号估。后台补一遍再刷新，
  // 用户不需要知道这件事，看到的就是准确的百分比。
  void api
    .backfillCharLens()
    .then((n) => {
      if (n > 0) void refresh();
    })
    .catch(() => {
      // 补算失败不影响读书，进度暂时仍是估算值
    });

  // ------------------------------------------------------------ 数据

  async function refresh(): Promise<void> {
    try {
      const [b, f] = await Promise.all([api.listBooks(), api.listFolders()]);
      books.set(b);
      folders.set(f);

      // 上次停留的目录可能在这次运行里被删掉了
      const fid = currentFolder.peek();
      if (fid !== null && !f.some((x) => x.id === fid)) {
        currentFolder.set(null);
        session.folderId = null;
      }

      // 首帧渲染完再回填滚动位置；只做一次，之后滚动是用户的。
      // books.set() 会同步跑完渲染 effect，所以这里的 DOM 已经就位。
      if (!restored) {
        restored = true;
        if (session.scrollTop > 0) {
          requestAnimationFrame(() => {
            grid.scrollTop = session.scrollTop;
          });
        }
      }
    } catch (err) {
      console.error("[shelf] 加载失败", err);
      showToast(`读取书架失败：${String(err)}`, "err");
    }
  }

  // ------------------------------------------------------------ 导入

  async function pickAndImport(): Promise<void> {
    try {
      const paths = await pickEbookFiles();
      await importFiles(paths);
    } catch (err) {
      showToast(`打开文件选择器失败：${String(err)}`, "err");
    }
  }

  /** 选一个文件夹：递归扫描，按磁盘子目录建分组（同名目录合并） */
  async function pickAndImportFolder(): Promise<void> {
    if (!isTauri()) {
      showToast("浏览器 preview 不支持选择文件夹", "err");
      return;
    }
    try {
      const dir = await pickDirectory("选择小说文件夹（递归扫描 txt / epub）");
      if (!dir) return;
      await importFolder(dir);
    } catch (err) {
      showToast(`打开文件夹选择器失败：${String(err)}`, "err");
    }
  }

  async function cancelImport(): Promise<void> {
    if (!busy() || cancelling()) return;
    cancelling.set(true);
    try {
      await api.cancelImport();
    } catch (err) {
      cancelling.set(false);
      showToast(`取消失败：${String(err)}`, "err");
    }
  }

  /** 打开「设置」面板（目前只有数据目录一项） */
  async function openDataDirSettings(): Promise<void> {
    let current = "";
    try {
      current = await api.dataDir();
    } catch (err) {
      showToast(String(err), "err");
      return;
    }

    const action = await showDataDirDialog({ current });
    if (action === "open") {
      try {
        await api.openDataDir();
      } catch (err) {
        showToast(String(err), "err");
      }
      return;
    }
    if (action === "change") {
      if (!isTauri()) {
        // 浏览器 preview 没有真实目录，也没有系统选择器
        showToast("浏览器 preview 不支持修改数据目录", "err");
        return;
      }
      await changeDataDir(current);
    }
  }

  /** 迁移/切换用户数据目录（书库、阅读偏好、窗口状态都在 library.db 里） */
  async function changeDataDir(current: string): Promise<void> {
    const picked = await pickDirectory();
    if (!picked || picked === current) return;

    const exists = await api.hasLibrary(picked).catch(() => false);
    if (exists) {
      const useIt = await confirmAction({
        title: "目录里已有书库",
        message: `「${picked}」里已经有 library.db。\n\n直接切换到这个书库吗？当前书库不会被复制过去。`,
        okText: "切换",
      });
      if (!useIt) return;
    } else {
      const ok = await confirmAction({
        title: "迁移数据目录",
        message: `把当前书库复制到：\n${picked}\n\n完成后应用会自动重启。原目录不会被删除。`,
        okText: "复制并重启",
      });
      if (!ok) return;
    }

    showToast("正在复制书库并重启…", "ok");
    void api
      .changeDataDir(picked, exists)
      .catch((err) => showToast(String(err), "err"));
  }

  async function importFiles(paths: string[]): Promise<void> {
    const list = filterEbooks(paths);
    if (list.length === 0) {
      if (paths.length > 0) showToast("只支持 txt / epub", "err");
      return;
    }

    // 当前选中了目录就导进去，否则放根目录
    const folderId = currentFolder.peek();
    await runImport(() => api.importPaths(list, folderId), { folderId, folderMode: false });
  }

  /** 文件夹导入：书按磁盘子目录分散，进度和取消复用同一套 */
  async function importFolder(dir: string): Promise<void> {
    const folderId = currentFolder.peek();
    await runImport(() => api.importFolder(dir, folderId), { folderId, folderMode: true });
  }

  /**
   * 文件 / 文件夹导入的公共收尾：进度条、取消、刷新、新书高亮、toast。
   * 两者的差异只有「调哪个 IPC」和「toast 里要不要说新建目录数」。
   */
  async function runImport(
    start: () => Promise<ImportReport>,
    opts: { folderId: number | null; folderMode: boolean },
  ): Promise<void> {
    busy.set(true);
    cancelling.set(false);
    importing.set(null);
    try {
      const report = await start();

      const added = report.imported.filter((i) => !i.alreadyExists);
      const dup = report.imported.length - added.length;
      const failed = report.failed.length;

      // 新书必须「落地可见」：先退出搜索态，再让第一本滚进视口并高亮 2 秒。
      // 否则导完只有一条 toast，用户还得自己翻半天——66 本的书架尤其难受。
      if (added.length > 0) {
        if (keyword.peek().trim()) keyword.set("");
        if (filter.peek() !== "all") filter.set("all");
        fresh.set(new Set(added.map((i) => i.id)));
      }

      await refresh();

      const parts: string[] = [];
      if (added.length > 0) {
        // 文件夹导入时书按子目录分散，不写「到「当前目录」」
        parts.push(
          opts.folderMode || opts.folderId === null
            ? `导入 ${added.length} 本`
            : `导入 ${added.length} 本到「${folderNameOf(opts.folderId)}」`,
        );
      }
      if (opts.folderMode && report.foldersCreated > 0) {
        parts.push(`新建 ${report.foldersCreated} 个目录`);
      }
      if (dup > 0) parts.push(`${dup} 本已在书架`);
      if (failed > 0) parts.push(`${failed} 本失败`);
      if (report.cancelled) parts.push(added.length > 0 ? "其余已取消" : "已取消");

      showToast(parts.join("，") || "没有可导入的文件", failed > 0 ? "err" : "ok");
      if (failed > 0) console.warn("[import] 失败明细\n" + report.failed.join("\n"));

      if (added.length > 0) {
        requestAnimationFrame(() => {
          const first = grid.querySelector<HTMLElement>(".card.is-new");
          if (!first) return;
          const top =
            first.getBoundingClientRect().top - grid.getBoundingClientRect().top + grid.scrollTop;
          grid.scrollTop = Math.max(0, top - 14);
        });
        window.setTimeout(() => fresh.set(new Set()), 2000);
      }
    } catch (err) {
      showToast(`导入失败：${String(err)}`, "err");
    } finally {
      busy.set(false);
      importing.set(null);
      cancelling.set(false);
    }
  }

  function showToast(text: string, tone: "ok" | "err"): void {
    toast.textContent = text;
    toast.classList.toggle("is-err", tone === "err");
    toast.classList.add("is-on");
    window.clearTimeout(toastTimer);
    toastTimer = window.setTimeout(() => toast.classList.remove("is-on"), 4200);
  }

  // ------------------------------------------------------------ 目录 / 书籍管理

  function folderNameOf(id: number | null): string {
    if (id === null) return "根目录";
    return folders().find((f) => f.id === id)?.name ?? "未知目录";
  }

  /** 与后端 validate_folder_name 对齐，先在本地拦一道，少一次往返 */
  function nameError(raw: string, parentId: number | null, selfId?: number): string | null {
    if (!raw) return "目录名不能为空";
    if (raw.length > 60) return "目录名太长（最多 60 字）";
    if (raw.includes("/") || raw.includes("\\")) return "目录名不能包含 / 或 \\";
    if (folders().some((f) => f.id !== selfId && f.parentId === parentId && f.name === raw)) {
      return `同级已有同名目录：${raw}`;
    }
    return null;
  }

  /** 目录选择列表：根目录 + 整棵树；移动目录时排除自己那棵子树 */
  function folderChoices(exclude?: number): ChooseItem<number | null>[] {
    const blocked = exclude === undefined ? null : new Set(subtreeOf(folders(), exclude));
    const items: ChooseItem<number | null>[] = [{ label: "根目录（不归类）", value: null }];
    for (const { node, depth } of flatten(folders(), new Set())) {
      if (blocked?.has(node.id)) continue;
      items.push({ label: node.name, value: node.id, depth: depth + 1 });
    }
    return items;
  }

  /** 侧栏「+」建的是当前目录的子目录；在根目录时等同于新建顶层目录 */
  function newFolderHere(): void {
    void newFolderDialog(currentFolder.peek());
  }

  async function newFolderDialog(parentId: number | null): Promise<void> {
    const level = parentId === null ? 1 : depthOf(folders(), parentId) + 1;
    if (level > MAX_FOLDER_DEPTH) {
      showToast(`目录最多 ${MAX_FOLDER_DEPTH} 级`, "err");
      return;
    }

    const name = await promptText({
      title: parentId === null ? "新建目录" : `在「${folderNameOf(parentId)}」下新建`,
      description:
        level === MAX_FOLDER_DEPTH ? `第 ${level} 级，已是最深一级` : `将创建第 ${level} 级目录`,
      placeholder: "目录名",
      okText: "创建",
      validate: (v) => nameError(v, parentId),
    });
    if (name === null) return;

    try {
      await api.createFolder(parentId, name);
      // 建在折叠的父目录里会「建完看不见」，顺手展开
      if (parentId !== null) {
        session.collapsed.delete(parentId);
        treeTick.update((n) => n + 1);
      }
      await refresh();
      showToast(`已创建「${name}」`, "ok");
    } catch (err) {
      showToast(String(err), "err");
    }
  }

  async function renameDialog(node: FolderNode): Promise<void> {
    const name = await promptText({
      title: "重命名目录",
      value: node.name,
      okText: "保存",
      validate: (v) => nameError(v, node.parentId, node.id),
    });
    if (name === null || name === node.name) return;

    try {
      await api.renameFolder(node.id, name);
      await refresh();
      showToast("已重命名", "ok");
    } catch (err) {
      showToast(String(err), "err");
    }
  }

  async function moveFolderDialog(node: FolderNode): Promise<void> {
    const picked = await chooseFolder<number | null>({
      title: `移动「${node.name}」`,
      description: `不能选它自己和它的子目录；移动后超过 ${MAX_FOLDER_DEPTH} 级会被拒绝`,
      items: folderChoices(node.id),
      current: node.parentId,
    });
    if (picked === undefined || picked === node.parentId) return;

    try {
      await api.moveFolder(node.id, picked);
      await refresh();
      showToast(`已移到「${folderNameOf(picked)}」`, "ok");
    } catch (err) {
      showToast(String(err), "err");
    }
  }

  async function deleteFolderDialog(node: FolderNode): Promise<void> {
    const ids = new Set(subtreeOf(folders(), node.id));
    const booksInside = books().filter((b) => b.folderId !== null && ids.has(b.folderId)).length;
    const subCount = ids.size - 1;

    let head = `将删除「${node.name}」`;
    if (subCount > 0) head += ` 及其下 ${subCount} 个子目录`;
    head += booksInside > 0 ? `，其中 ${booksInside} 本书会一并从书架移除。` : "。";

    const ok = await confirmAction({
      title: "删除目录",
      message: `${head}\n\n只删除书架中的记录，磁盘上的原文件不会被删除。`,
      okText: "删除",
      danger: true,
    });
    if (!ok) return;

    try {
      const r = await api.deleteFolder(node.id);
      // 正在浏览的目录如果就在被删的子树里，得退回「全部书籍」
      const cur = currentFolder.peek();
      if (cur !== null && ids.has(cur)) {
        currentFolder.set(null);
        filter.set("all");
      }
      for (const id of ids) session.collapsed.delete(id);
      await refresh();
      showToast(`已删除 ${r.foldersRemoved} 个目录、${r.booksRemoved} 本书的记录`, "ok");
    } catch (err) {
      showToast(String(err), "err");
    }
  }

  function bookMenu(book: BookSummary, x: number, y: number): void {
    showContextMenu(x, y, [
      {
        label: "移动到…",
        disabled: book.missing,
        hint: book.missing ? "文件已失效" : undefined,
        onClick: () => void moveBookDialog(book),
      },
      { sep: true },
      { label: "从书架移除", danger: true, onClick: () => void removeBookDialog(book) },
    ]);
  }

  async function moveBookDialog(book: BookSummary): Promise<void> {
    const picked = await chooseFolder<number | null>({
      title: `移动「${book.title}」`,
      items: folderChoices(),
      current: book.folderId,
    });
    if (picked === undefined || picked === book.folderId) return;
    await moveBookTo(book, picked);
  }

  // ------------------------------------------------------------ 排序

  function openSortMenu(): void {
    const rect = sortBtn.getBoundingClientRect();
    const items: MenuItem[] = SORTS.map((s) => ({
      label: s.label,
      checked: s.key === sort.peek(),
      onClick: () => {
        sort.set(s.key);
        saveShelfSort(s.key);
      },
    }));
    items.push({ sep: true });
    items.push({
      label: "倒序排列",
      checked: sortDesc.peek(),
      onClick: () => {
        const next = !sortDesc.peek();
        sortDesc.set(next);
        saveShelfSortDesc(next);
      },
    });
    // 按钮左对齐弹菜单，溢出视口时 showContextMenu 会自己收回来
    showContextMenu(rect.left, rect.bottom + 6, items);
  }

  // ------------------------------------------------------------ 拖动

  const DROP_MARKS = ["is-drop-hover", "drop-before", "drop-after"];

  function clearMarks(el: Element | null): void {
    if (!el) return;
    for (const c of DROP_MARKS) el.classList.remove(c);
  }

  function toggleCollapse(id: number): void {
    if (session.collapsed.has(id)) session.collapsed.delete(id);
    else session.collapsed.add(id);
    treeTick.update((n) => n + 1);
  }

  function dragBook(e: PointerEvent, book: BookSummary): void {
    if (book.missing) return;
    startDrag({
      start: e,
      label: book.title,
      selector: "[data-folder-id], [data-drop-root]",
      onHover: (next, prev) => {
        clearMarks(prev);
        if (!next) return;
        // 拖回它现在所在的目录没有意义，别给高亮
        if (folderOf(next.el) !== book.folderId) next.el.classList.add("is-drop-hover");
      },
      onDrop: (t) => {
        const folderId = folderOf(t.el);
        if (folderId !== book.folderId) void moveBookTo(book, folderId);
      },
    });
  }

  async function moveBookTo(book: BookSummary, folderId: number | null): Promise<void> {
    try {
      await api.setBookFolder(book.id, folderId);
      await refresh();
      showToast(`已移到「${folderNameOf(folderId)}」`, "ok");
    } catch (err) {
      showToast(String(err), "err");
    }
  }

  /** 投放目标对应的目录 id；`data-folder-id` 缺失表示根目录 */
  function folderOf(el: Element): number | null {
    const raw = el.getAttribute("data-folder-id");
    return raw === null ? null : Number(raw);
  }

  function dragFolder(e: PointerEvent, node: FolderNode): void {
    startDrag({
      start: e,
      label: node.name,
      selector: "[data-folder-id]",
      onHover: (next, prev) => {
        clearMarks(prev);
        if (!next) return;
        const id = folderOf(next.el);
        if (id === node.id) return;
        markFolderDrop(next, node);
      },
      onDrop: (t) => void dropFolder(node, t),
    });
  }

  /** 同级靠上下边缘 = 插到前/后；其余情况 = 放进目标目录 */
  function markFolderDrop(t: DragTarget, dragged: FolderNode): void {
    const target = folders().find((f) => f.id === folderOf(t.el));
    if (!target) return;
    if (target.parentId === dragged.parentId && t.zone !== "middle") {
      t.el.classList.add(t.zone === "bottom" ? "drop-after" : "drop-before");
    } else {
      t.el.classList.add("is-drop-hover");
    }
  }

  async function dropFolder(node: FolderNode, t: DragTarget): Promise<void> {
    const target = folders().find((f) => f.id === folderOf(t.el));
    if (!target || target.id === node.id) return;

    try {
      if (target.parentId === node.parentId && t.zone !== "middle") {
        await api.reorderFolder(node.id, target.id, t.zone === "bottom");
        await refresh();
        showToast("已调整顺序", "ok");
      } else {
        await api.moveFolder(node.id, target.id);
        await refresh();
        showToast(`已移到「${target.name}」下`, "ok");
      }
    } catch (err) {
      showToast(String(err), "err");
    }
  }

  async function removeBookDialog(book: BookSummary): Promise<void> {
    const ok = await confirmAction({
      title: "从书架移除",
      message: `「${book.title}」会从书架中移除。\n\n只删除书架里的记录，磁盘上的原文件不会被删除。`,
      okText: "移除",
      danger: true,
    });
    if (!ok) return;

    try {
      await api.deleteBook(book.id);
      await refresh();
      showToast(`已移除「${book.title}」`, "ok");
    } catch (err) {
      showToast(String(err), "err");
    }
  }

  // 拖拽导入（仅 Tauri 环境）
  void onEbookDrag((phase, paths) => {
    if (phase === "over") {
      dropMask.classList.add("is-on");
    } else if (phase === "leave") {
      dropMask.classList.remove("is-on");
    } else {
      dropMask.classList.remove("is-on");
      void importFiles(paths);
    }
  }).then((off) => {
    if (token !== instanceSeq) {
      off();
      return;
    }
    dropUnlisten = off;
  });

  // 导入进度（后端线程逐本 emit，前端只负责显示"第几本"）
  void onImportProgress((p) => {
    if (token !== instanceSeq) return;
    importing.set({ done: p.done, total: p.total, title: p.title });
  }).then((off) => {
    if (token !== instanceSeq) {
      off();
      return;
    }
    progressUnlisten?.();
    progressUnlisten = off;
  });

  // ------------------------------------------------------------ 结构

  const sortBtn = h("button", {
    class: "sort-btn",
    type: "button",
    title: "排序方式",
    onClick: () => openSortMenu(),
  });
  effect(() => {
    const label = SORTS.find((s) => s.key === sort())?.label ?? "排序";
    sortBtn.textContent = `${label} ▾`;
  });

  const search = h("input", {
    class: "search",
    type: "search",
    placeholder: "搜索书名 / 作者（全库）",
    onInput: (e: Event) => keyword.set((e.target as HTMLInputElement).value),
  });
  // 「清除搜索」只改 signal，输入框要跟着同步回来
  effect(() => {
    const k = keyword();
    if (search.value !== k) search.value = k;
  });

  // 应用级设置（数据目录）的常驻入口。之前只有侧栏底部一行 12px 灰字，几乎看不见。
  const settingsBtn = h(
    "button",
    {
      class: "settings-btn",
      type: "button",
      title: "设置：数据目录",
      onClick: () => void openDataDirSettings(),
    },
    h("span", { class: "settings-icon" }, "⚙"),
    "设置",
  );

  // Ctrl+F 聚焦书架搜索框，替代 WebView2 的查找栏
  const onSearchKey = (e: KeyboardEvent): void => {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
      e.preventDefault();
      search.focus();
      search.select();
    }
  };
  document.addEventListener("keydown", onSearchKey);
  searchKeyUnlisten = () => document.removeEventListener("keydown", onSearchKey);

  return h(
    "div",
    { class: "app" },
    h(
      "aside",
      { class: "sidebar" },
      h(
        "div",
        { class: "brand" },
        h("span", { class: "brand-mark" }),
        h("span", { class: "brand-text" }, "我的书架"),
      ),
      navList,
      h(
        "div",
        { class: "section-title" },
        h("span", null, "目录"),
        h(
          "button",
          {
            class: "section-add",
            type: "button",
            title: "新建目录",
            onClick: () => newFolderHere(),
          },
          "+",
        ),
      ),
      treeList,
      h("div", { class: "sidebar-foot" }, importBtn, importFolderBtn, importPanel),
    ),
    h(
      "main",
      { class: "content" },
      h(
        "header",
        { class: "topbar" },
        h("div", { class: "topbar-left" }, titleEl, scopeEl),
        h("div", { class: "topbar-right" }, sortBtn, search, countEl, settingsBtn),
      ),
      continueHost,
      grid,
    ),
    dropMask,
    toast,
  );
}

// ---------------------------------------------------------------- helpers

interface ViewQuery {
  all: BookSummary[];
  filter: Filter;
  keyword: string;
  /** 当前目录整棵子树的 id；null = 不按目录过滤 */
  folderScope: Set<number> | null;
  sort: SortKey;
  desc: boolean;
}

function visible(q: ViewQuery): BookSummary[] {
  const kw = q.keyword.trim().toLowerCase();
  let list: BookSummary[];

  if (kw) {
    // 搜索**永远跨全部目录**：只在当前目录里找，会让用户以为书丢了。
    // 这也是为什么搜索结果的卡片要显示目录全路径。
    list = q.all.filter(
      (b) => b.title.toLowerCase().includes(kw) || (b.author ?? "").toLowerCase().includes(kw),
    );
  } else {
    list = q.all;
    if (q.folderScope) {
      const scope = q.folderScope;
      list = list.filter((b) => b.folderId !== null && scope.has(b.folderId));
    }
    if (q.filter === "recent") {
      list = list.filter((b) => b.lastReadAt !== null);
    } else if (q.filter === "unread") {
      list = list.filter((b) => b.progress === 0);
    }
  }

  return sortBooks(list, q.sort, q.desc);
}

/** 排序不改原数组：books() 的引用是 refresh 的判等依据，就地 sort 会让它失真。 */
function sortBooks(list: BookSummary[], key: SortKey, desc: boolean): BookSummary[] {
  const out = [...list];

  switch (key) {
    case "title":
      out.sort((a, b) => a.title.localeCompare(b.title, "zh-Hans-CN"));
      break;
    case "recent":
      out.sort((a, b) => (b.lastReadAt ?? 0) - (a.lastReadAt ?? 0));
      break;
    case "progress":
      out.sort((a, b) => b.progress - a.progress);
      break;
    case "chapters":
      out.sort((a, b) => b.chapterCount - a.chapterCount);
      break;
    case "added":
      out.sort((a, b) => b.addedAt - a.addedAt);
      break;
    default:
      // 最近活动：读过的看阅读时间，没读过的看入库时间
      out.sort((a, b) => (b.lastReadAt ?? b.addedAt) - (a.lastReadAt ?? a.addedAt));
      break;
  }

  // 每个选项都有自然方向，「倒序」只是把它翻过来
  if (desc) out.reverse();
  return out;
}

/** 扁平化目录树，depth 用于缩进；折叠的子树整段不产出。 */
function flatten(
  folders: FolderNode[],
  collapsed: Set<number>,
): { node: FolderNode; depth: number; hasChildren: boolean }[] {
  const children = new Map<number | null, FolderNode[]>();
  for (const f of folders) {
    const bucket = children.get(f.parentId);
    if (bucket) bucket.push(f);
    else children.set(f.parentId, [f]);
  }
  for (const bucket of children.values()) bucket.sort((a, b) => a.sort - b.sort);

  const out: { node: FolderNode; depth: number; hasChildren: boolean }[] = [];
  const walk = (parent: number | null, depth: number): void => {
    for (const node of children.get(parent) ?? []) {
      const kids = children.get(node.id) ?? [];
      out.push({ node, depth, hasChildren: kids.length > 0 });
      if (kids.length > 0 && !collapsed.has(node.id)) walk(node.id, depth + 1);
    }
  };
  walk(null, 0);
  return out;
}

/** 目录自己的层级：根目录下新建的是第 1 级 */
function depthOf(all: FolderNode[], id: number): number {
  let level = 1;
  let cur = all.find((f) => f.id === id);
  for (let guard = 0; cur && cur.parentId !== null && guard <= MAX_FOLDER_DEPTH; guard++) {
    const parentId: number = cur.parentId;
    cur = all.find((f) => f.id === parentId);
    if (cur) level += 1;
  }
  return level;
}

/** 某目录及其全部后代的 id，按层展开 */
function subtreeOf(all: FolderNode[], root: number): number[] {
  const ids = [root];
  for (let i = 0; i < ids.length; i++) {
    for (const f of all) {
      if (f.parentId === ids[i]) ids.push(f.id);
    }
  }
  return ids;
}

/**
 * 每个目录里的书数。**含子目录**：父目录显示 0、子目录里却有 12 本，
 * 会让人以为目录树坏了。
 */
function folderCounts(all: FolderNode[], books: BookSummary[]): Map<number, number> {
  const parentOf = new Map(all.map((f) => [f.id, f.parentId] as const));
  const counts = new Map<number, number>();

  for (const b of books) {
    let id = b.folderId;
    for (let guard = 0; id !== null && guard <= MAX_FOLDER_DEPTH; guard++) {
      counts.set(id, (counts.get(id) ?? 0) + 1);
      id = parentOf.get(id) ?? null;
    }
  }
  return counts;
}

/** 目录 id → 「玄幻 / 仙侠 / 古典仙侠」 */
function folderPaths(all: FolderNode[]): Map<number, string> {
  const byId = new Map(all.map((f) => [f.id, f] as const));
  const out = new Map<number, string>();

  for (const f of all) {
    const parts: string[] = [];
    let cur: FolderNode | undefined = f;
    for (let guard = 0; cur && guard <= MAX_FOLDER_DEPTH; guard++) {
      parts.unshift(cur.name);
      cur = cur.parentId === null ? undefined : byId.get(cur.parentId);
    }
    out.set(f.id, parts.join(" / "));
  }
  return out;
}

type CardState = "unread" | "reading" | "done";

function cardState(b: BookSummary): CardState {
  if (b.progress >= 0.999) return "done";
  return b.progress > 0 ? "reading" : "unread";
}

/** 「第 312 章」用的是真实的 chapter_idx，不是把百分比乘回去估的 */
function chapterLabel(b: BookSummary): string {
  const state = cardState(b);
  if (state === "unread") return "未开始";
  if (state === "done") return "已读完";
  return b.lastChapter === null ? "已读" : `第 ${b.lastChapter + 1} 章`;
}

/** 卡片左下：当前章 / 总章数。总章数始终可见，未读和读完时不显示"第 0 章" */
function chapterStat(b: BookSummary): string {
  const state = cardState(b);
  if (state === "unread" || state === "done" || b.lastChapter === null) {
    return `共 ${b.chapterCount} 章`;
  }
  return `${b.lastChapter + 1}/${b.chapterCount} 章`;
}

/** 字数只用于展示，保留一位小数；老库没回填 char_len 时返回空串 */
function formatChars(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "";
  if (n >= 100_000_000) return `${(n / 100_000_000).toFixed(1).replace(/\.0$/, "")} 亿字`;
  if (n >= 10_000) return `${(n / 10_000).toFixed(1).replace(/\.0$/, "")} 万字`;
  return `${n} 字`;
}

function timeAgo(ts: number | null): string {
  if (!ts) return "";
  const diff = Math.floor(Date.now() / 1000) - ts;
  if (diff < 60) return "刚刚";
  if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`;
  if (diff < 86_400) return `${Math.floor(diff / 3600)} 小时前`;
  const days = Math.floor(diff / 86_400);
  if (days === 1) return "昨天";
  if (days < 30) return `${days} 天前`;
  const months = Math.floor(days / 30);
  if (months < 12) return `${months} 个月前`;
  return `${Math.floor(days / 365)} 年前`;
}

/** 最近在读、且没读完的一本；没有就返回 null（继续阅读条不出现） */
function continueBook(list: BookSummary[]): BookSummary | null {
  let best: BookSummary | null = null;
  for (const b of list) {
    if (b.missing || b.lastReadAt === null) continue;
    if (b.progress <= 0 || b.progress >= 0.999) continue;
    if (!best || b.lastReadAt > (best.lastReadAt ?? 0)) best = b;
  }
  return best;
}

function ContinueCard(book: BookSummary, onOpen: () => void): HTMLElement {
  const pct = Math.round(book.progress * 100);
  return h(
    "button",
    { class: "continue", type: "button", onClick: onOpen },
    h("span", { class: "continue-mark" }),
    h(
      "span",
      { class: "continue-body" },
      h(
        "span",
        { class: "continue-line" },
        h("span", { class: "continue-kicker" }, "继续阅读"),
        h("span", { class: "continue-title" }, book.title),
      ),
      h(
        "span",
        { class: "continue-meta" },
        `${chapterLabel(book)} · ${pct}% · ${timeAgo(book.lastReadAt)}`,
      ),
    ),
    h("span", { class: "continue-go" }, "继续"),
  );
}

interface CardOptions {
  /** 搜索态下才有：这本书所在的目录全路径 */
  path: string | null;
  fresh: boolean;
  onOpen: (book: BookSummary) => void;
  onMenu: (book: BookSummary, x: number, y: number) => void;
  onDrag: (book: BookSummary, e: PointerEvent) => void;
}

function BookCard(b: BookSummary, o: CardOptions): HTMLElement {
  const pct = Math.round(b.progress * 100);
  const state = cardState(b);
  const rawName = b.filePath.split(/[\\/]/).pop() || b.title;
  // 展示名去掉扩展名（.txt / .epub）；完整路径仍在 title 提示里
  const fileName = rawName.replace(/\.[^.]+$/, "") || b.title;

  const card = h(
    "button",
    {
      class: `card is-${state}${b.missing ? " is-missing" : ""}${o.fresh ? " is-new" : ""}`,
      title: `${b.filePath}\n${b.chapterCount} 章${
        b.totalChars > 0 ? ` · ${formatChars(b.totalChars)}` : ""
      }\n${b.missing ? "文件已不在原位" : "拖动可放进目录，右键可管理"}`,
      onPointerDown: (e: Event) => o.onDrag(b, e as PointerEvent),
      onClick: () => o.onOpen(b),
      onContextMenu: (e: Event) => {
        e.preventDefault();
        const me = e as MouseEvent;
        o.onMenu(b, me.clientX, me.clientY);
      },
    },
    // 搜索时把目录放在书名上方：这是区分同名书 / 分卷的第一信息
    o.path ? h("span", { class: "card-path", title: o.path }, o.path) : null,
    b.missing ? h("span", { class: "missing-badge" }, "文件失效") : null,
    h("span", { class: "card-title" }, fileName),
    h(
      "span",
      { class: "card-meta" },
      h("span", { class: "card-author" }, b.author ?? "未知作者"),
      b.totalChars > 0 ? h("span", { class: "card-words" }, formatChars(b.totalChars)) : null,
    ),
    h(
      "span",
      { class: "card-stat" },
      h("span", null, chapterStat(b)),
      h(
        "span",
        { class: "card-pct" },
        state === "unread" ? "未开始" : state === "done" ? "已读完" : `${pct}%`,
      ),
    ),
    h("span", { class: "bar" }, state === "unread" ? null : h("i", { style: { width: `${pct}%` } })),
  );

  // 右键是唯一管理入口太隐蔽了：hover 时右上角出一个「⋯」，点开同一个菜单。
  // 卡片本身必须是 <button>（键盘可达），嵌套 button 是非法 HTML，
  // 所以「⋯」做成绝对定位的兄弟节点，外面再套一层定位容器。
  const more = h(
    "button",
    {
      class: "card-more",
      type: "button",
      title: "移动 / 移除",
      "aria-label": `管理 ${fileName}`,
      onClick: (e: Event) => {
        e.stopPropagation();
        const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
        o.onMenu(b, r.right - 6, r.bottom + 2);
      },
    },
    "⋯",
  );

  return h("div", { class: `card-wrap${b.missing ? " is-missing" : ""}` }, card, more);
}

type EmptyReason = "empty" | "search" | "filter";

const EMPTY_TEXT: Record<EmptyReason, { title: string; sub: string; action: string }> = {
  empty: { title: "书架还是空的", sub: "点左下角导入，或直接把 txt / epub 拖进窗口", action: "" },
  search: {
    title: "全库没有匹配的书",
    sub: "已经在所有目录里找过了，换个书名或作者再试试",
    action: "清除搜索",
  },
  filter: { title: "这里暂时没有书", sub: "当前筛选或目录下没有书籍", action: "显示全部" },
};

function EmptyState(reason: EmptyReason, onReset: () => void): HTMLElement {
  const t = EMPTY_TEXT[reason];
  return h(
    "div",
    { class: "empty" },
    h("div", { class: "empty-icon" }),
    h("p", { class: "empty-title" }, t.title),
    h("p", { class: "empty-sub" }, t.sub),
    reason === "empty"
      ? null
      : h("button", { class: "empty-reset", onClick: onReset }, t.action),
  );
}
