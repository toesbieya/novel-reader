import { clear, h } from "../core/dom";
import { showContextMenu } from "../core/menu";
import { api, isTauri, pickFontFile, type ChapterContent, type ChapterMeta, type SearchHit } from "../core/ipc";
import { Paginator } from "../reader/paginator";
import { getReaderPrefs, saveReaderPrefs } from "../core/settings";
import {
  DEFAULT_PREFS,
  FONT_SIZE_MAX,
  FONT_SIZE_MIN,
  LINE_HEIGHT_MAX,
  LINE_HEIGHT_MIN,
  LINE_HEIGHT_PRESETS,
  LINE_HEIGHT_STEP,
  THEME_LABELS,
  clampLineHeight,
  type ReaderFont,
  type ReaderTheme,
} from "../reader/prefs";
import { registerUserFont, unregisterUserFont, USER_FONT_FAMILY } from "../reader/user-font";

const TOC_ROW_H = 34;
const SAVE_DELAY = 600;
/** 滚轮累计多少像素翻一页；连续滚动会连续翻，不再按时间锁 */
const WHEEL_STEP = 100;
/** 停顿超过这么久就清掉滚轮余量，避免下一次轻碰突然翻页 */
const WHEEL_RESET_MS = 200;
/** 鼠标静止多久后收起顶栏/底栏 */
const IDLE_DELAY = 200;
/** 点击正文中间唤出工具栏后多停一会儿：200ms 连按钮都来不及看清 */
const CLICK_CHROME_HOLD = 1600;
/** 边距滑块的下限与步长（1px：滑块精度靠右侧数字框补） */
const MARGIN_MIN = 32;
const MARGIN_STEP = 1;
/**
 * 点击翻页的左右热区：各占视口宽 30%，中间 40% 唤出工具栏。
 * 按 `.page-viewport` 的外框算，左右留白区也算热区——点留白翻页符合直觉。
 */
const CLICK_SIDE_RATIO = 0.3;
/** 按住超过这么久再松手不算单击，避免长按选词时误翻 */
const CLICK_MAX_HOLD = 700;
/**
 * 边距拉到最大时正文至少留这么宽（默认字号下约 6 个汉字），
 * 这样能把正文挤成一条窄竖列、像竖排书那样读。再窄 columnWidth 会退化到 0、整页空白。
 */
const MIN_CONTENT_W = 120;
/** 底栏章节滑杆的圆点宽度：填充末端与 hover 预览的位置换算都按它对齐 */
const SLIDER_THUMB = 14;

type DrawerKind = "toc" | "search" | "settings";

const DRAWER_TITLES: Record<DrawerKind, string> = {
  toc: "目录",
  search: "全文搜索",
  settings: "阅读设置",
};

/** 系统字体列表整个会话只枚举一次（浏览器模式由 mock 提供示例） */
let systemFontsPromise: Promise<string[]> | null = null;

function loadSystemFonts(): Promise<string[]> {
  if (!systemFontsPromise) {
    systemFontsPromise = api.listSystemFonts().catch((err) => {
      console.warn("[reader] 读取系统字体失败", err);
      return [];
    });
  }
  return systemFontsPromise;
}

export function ReaderView(bookId: number, bookTitle: string, onExit: () => void): HTMLElement {
  const paginator = new Paginator();
  const prefs = getReaderPrefs();
  // 旧库 / 手改 db 的行距可能不在 0.1 档位上，先归一，± 步进才不会在浮点毛刺上漂移
  prefs.lineHeight = clampLineHeight(prefs.lineHeight);

  let chapters: ChapterMeta[] = [];
  let chapterIndex = 0;
  let disposed = false;
  let saveTimer = 0;
  let idleTimer = 0;
  let loadSeq = 0;
  /** 边距拖动：整个手势共用一个字符锚点，避免版式连续变化把阅读位置带偏 */
  let marginAnchor: number | null = null;
  let marginRaf = 0;
  let searchHits: SearchHit[] = [];
  let searchQuery = "";
  /** 用户点击 / 上下键选中的那一条命中；高亮要画在它身上，而不是本章第一条 */
  let activeHit: SearchHit | null = null;
  /** 搜索高亮是否还该画：点结果时开，翻页 / 跳章 / 点正文中间时关 */
  let highlightActive = false;
  /** 与 searchHits 一一对应的结果行，用于键盘切换时标选中态 */
  let searchRowEls: HTMLElement[] = [];
  let searchTimer = 0;
  /** 每页拉多少条命中；total 可能十几万，「加载更多」按这个步长翻 */
  const SEARCH_PAGE = 200;
  /** 全书精确命中数，和已加载的 searchHits 分开记 */
  let searchTotal = 0;
  /** 搜索抽屉的延迟聚焦：关抽屉 / 离开阅读页时必须清掉 */
  let searchFocusTimer = 0;
  /** 「加载更多」进行中，避免重复点击叠加请求 */
  let searchLoadingMore = false;
  /** 上一批最后一条命中所在的章节：跨批次追加时不要重复插组头 */
  let lastGroupChapter = -1;
  /** 目录筛选关键词，空串 = 不过滤 */
  let tocFilter = "";
  /** 过滤后命中的原章节下标，滚动时复用，避免每帧重算 */
  let tocMatches: number[] = [];
  /** 开始筛选前的滚动位置，清空关键词后恢复 */
  let tocSavedScroll = 0;
  /** 边距滑块与数值：窗口尺寸变了要同步上限和显示值 */
  let marginSlider: HTMLInputElement | null = null;
  /** 边距数字输入框：1px 精度靠它，滑块只做粗调 */
  let marginNumberEl: HTMLInputElement | null = null;
  /** 上次算出的边距上限；只有它变化时才动滑块，避免拖滑块时反复回写 */
  let marginCap = 0;
  /** 按下位置：位移超过阈值说明是在拖选文本，不是点击 */
  let clickStart: { x: number; y: number; at: number } | null = null;

  const ac = new AbortController();

  // ------------------------------------------------------------ 顶栏 / 底栏

  const titleEl = h("div", { class: "reader-chapter", title: bookTitle }, bookTitle);
  const pageInfo = h("span", { class: "page-info" }, "");
  const chapterInfo = h("span", { class: "chapter-info" }, "");

  const chapterSlider = h("input", {
    class: "chapter-slider",
    type: "range",
    min: "0",
    max: "0",
    value: "0",
    "aria-label": "章节进度，拖动跳转",
    // 拖动途中只更新填充色（纯 CSS 变量，不碰正文）；松手才真正加载章节。
    // 若把跳章放到 input 上，一次拖动会触发上百次 read_chapter + 全章重建，正文一路闪。
    onInput: (e: Event) => {
      const v = Number((e.target as HTMLInputElement).value);
      if (Number.isFinite(v)) setSliderFill(v);
    },
    onChange: (e: Event) => {
      const v = Number((e.target as HTMLInputElement).value);
      // 跳章后立刻记一次进度：否则直接关窗时这次跳转不会被保存
      if (Number.isFinite(v)) {
        clearSearchHighlight();
        void openChapter(v, 0).then((ok) => { if (ok) scheduleSave(); });
      }
    },
  });
  const barTip = h("div", { class: "bar-tip" }, "");

  const topBar = h(
    "header",
    { class: "reader-bar top" },
    h("button", { class: "icon-btn", title: "返回书架（Esc）", onClick: () => leave() }, "←"),
    h("button", { class: "text-btn", onClick: () => openDrawer("toc") }, "目录"),
    // 书名常驻顶栏：同时读几本书时，光看章节名不知道自己在哪本里
    h("div", { class: "reader-book", title: bookTitle }, bookTitle),
    h("div", { class: "reader-spacer" }),
    h(
      "div",
      { class: "reader-actions" },
      h("button", { class: "text-btn", onClick: () => openDrawer("search") }, "搜索"),
      h("button", { class: "text-btn", onClick: () => openDrawer("settings") }, "设置"),
    ),
  );

  const bottomBar = h(
    "footer",
    { class: "reader-bar bottom" },
    h("button", { class: "text-btn", onClick: () => turn(-1) }, "‹ 上一页"),
    h("div", { class: "bar-progress" }, pageInfo, chapterSlider, chapterInfo),
    h("button", { class: "text-btn", onClick: () => turn(1) }, "下一页 ›"),
  );

  // ------------------------------------------------------------ 三块面板内容

  const tocScroll = h("div", { class: "toc-scroll" });
  const tocInner = h("div", { class: "toc-inner" });
  tocScroll.appendChild(tocInner);
  tocScroll.addEventListener("scroll", () => renderTocWindow(), { passive: true });

  const tocInput = h("input", {
    class: "search-input",
    type: "search",
    placeholder: "按章节名筛选，如「第3000」",
    onInput: (e: Event) => applyTocFilter((e.target as HTMLInputElement).value.trim()),
  });
  const tocPanel = h("div", { class: "toc-panel" }, tocInput, tocScroll);

  const searchInput = h("input", {
    class: "search-input",
    type: "search",
    placeholder: "搜索全书…",
    // 键盘导航不占正文：提示放在 title 里，meta 行只留计数和耗时
    title: "↑↓ 在结果间切换",
    onInput: () => {
      window.clearTimeout(searchTimer);
      searchTimer = window.setTimeout(() => void runSearch(), 260);
    },
    onKeyDown: (e: KeyboardEvent) => {
      if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
      if (searchHits.length === 0) return;
      e.preventDefault();
      const current = activeHit ? searchHits.indexOf(activeHit) : -1;
      const dir = e.key === "ArrowDown" ? 1 : -1;
      const next = current < 0 ? (dir > 0 ? 0 : searchHits.length - 1) : current + dir;
      if (next !== current) selectSearchHit(next);
    },
  });
  const searchMeta = h("div", { class: "panel-meta" }, "");
  // 点击结果不再自动收抽屉，必须留一个显式出口：不然点完一条就只剩 Esc 和遮罩
  const searchClose = h(
    "button",
    { class: "panel-close", type: "button", title: "关闭搜索（Esc）", onClick: () => closeDrawers() },
    "关闭",
  );
  const searchMetaRow = h("div", { class: "panel-meta-row" }, searchMeta, searchClose);
  const searchList = h("div", { class: "search-list" });
  const searchMore = h(
    "button",
    { class: "search-more", onClick: () => void loadMoreHits() },
    "加载更多",
  );
  searchMore.hidden = true;
  const searchPanel = h(
    "div",
    { class: "panel-body" },
    searchInput,
    searchMetaRow,
    searchList,
    searchMore,
  );

  const settingsPanel = buildSettings();

  const panels: Record<DrawerKind, HTMLElement> = {
    toc: tocPanel,
    search: searchPanel,
    settings: settingsPanel,
  };

  // ------------------------------------------------------------ 单一抽屉

  const drawerTitle = h("span", { class: "drawer-title" }, "");
  const drawerBody = h("div", { class: "drawer-body" });

  const drawerEl = h(
    "aside",
    // 默认停在右侧屏幕外。少了这个初始类，绝对定位元素会退回静态位置，
    // 直接盖在正文左边。
    { class: "drawer is-right" },
    h(
      "div",
      { class: "drawer-head" },
      drawerTitle,
      h("button", { class: "icon-btn", onClick: () => closeDrawers() }, "×"),
    ),
    drawerBody,
  );

  const backdrop = h("div", { class: "drawer-backdrop", onClick: () => closeDrawers() });

  // ------------------------------------------------------------ 根节点

  /**
   * 把已读段画到圆点中心。原生 range 的圆点只在 [thumb/2, 宽 - thumb/2] 之间移动，
   * 直接用百分比会和圆点错开半个圆点，所以按实际像素算；
   * 窗口尺寸变化后 `updateChrome()` 会重算一次。
   */
  function setSliderFill(value: number): void {
    const max = Number(chapterSlider.max) || 0;
    const ratio = max > 0 ? Math.min(1, Math.max(0, value / max)) : 0;
    const usable = Math.max(0, chapterSlider.clientWidth - SLIDER_THUMB);
    chapterSlider.style.setProperty("--fill", `${SLIDER_THUMB / 2 + ratio * usable}px`);
  }

  /** 底部章节滑杆：hover / 拖动时提示将跳到哪一章 */
  function showChapterTip(clientX: number): void {
    if (chapters.length === 0) return;

    const rect = chapterSlider.getBoundingClientRect();
    // 圆点本身占两端，按可用宽度折算（尺寸见 SLIDER_THUMB），肉眼看位置够准
    const thumb = SLIDER_THUMB;
    const usable = Math.max(1, rect.width - thumb);
    const ratio = Math.min(1, Math.max(0, (clientX - rect.left - thumb / 2) / usable));
    const idx = Math.round(ratio * (chapters.length - 1));

    barTip.textContent = chapters[idx].title;
    barTip.classList.add("is-on");
    barTip.style.top = `${rect.top}px`;

    // 先测宽度再夹到视口内，避免提示在屏幕边缘被裁掉
    const half = barTip.offsetWidth / 2;
    const left = Math.min(Math.max(clientX, half + 8), window.innerWidth - half - 8);
    barTip.style.left = `${left}px`;
  }

  function hideChapterTip(): void {
    barTip.classList.remove("is-on");
  }

  chapterSlider.addEventListener("pointerenter", (e) => showChapterTip(e.clientX), {
    signal: ac.signal,
  });
  chapterSlider.addEventListener("pointermove", (e) => showChapterTip(e.clientX), {
    signal: ac.signal,
  });
  chapterSlider.addEventListener("pointerdown", (e) => showChapterTip(e.clientX), {
    signal: ac.signal,
  });
  chapterSlider.addEventListener("pointerleave", hideChapterTip, { signal: ac.signal });

  // 左右翻页按钮：鼠标进热区时从边缘滑出，可直接点击翻页
  const leftHint = h("button", {
    class: "page-hint is-left",
    type: "button",
    tabindex: "-1",
    "aria-label": "上一页",
    onClick: () => turn(-1),
  });
  const rightHint = h("button", {
    class: "page-hint is-right",
    type: "button",
    tabindex: "-1",
    "aria-label": "下一页",
    onClick: () => turn(1),
  });

  const root = h(
    "div",
    { class: "reader" },
    topBar,
    titleEl,
    paginator.el,
    leftHint,
    rightHint,
    bottomBar,
    barTip,
    backdrop,
    drawerEl,
  );

  applyPrefs();
  void boot();

  // ------------------------------------------------------------ 工具栏自动隐藏

  /** 顶部/底部这么高的范围算"边缘"，指针进到这里才唤出工具栏 */
  const EDGE_BAND = 64;
  let pointerY = -1;

  /**
   * 静止 2 秒收起顶栏和底栏，给正文让出视觉空间。
   *
   * 只改 opacity/transform，不动 flex 布局——栏位一旦塌陷就会触发 ResizeObserver
   * 重排，每次呼吸都跳页，那是灾难。
   */
  function pokeChrome(holdMs = IDLE_DELAY): void {
    root.classList.remove("is-idle");
    window.clearTimeout(idleTimer);

    if (root.dataset.drawer) return;

    idleTimer = window.setTimeout(() => {
      if (root.dataset.drawer) return;
      const active = document.activeElement;
      const tag = active?.tagName;
      if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
      // 指针正压在工具栏上时不收：否则盯着"目录"按钮看两秒，按钮就自己消失了
      if (pointerY >= 0 && overBar(pointerY)) return;
      root.classList.add("is-idle");
    }, holdMs);
  }

  function overBar(y: number): boolean {
    return [topBar, bottomBar].some((bar) => {
      const r = bar.getBoundingClientRect();
      return y >= r.top && y <= r.bottom;
    });
  }

  /**
   * 只有指针移动、且靠近上下边缘才唤出工具栏。正文中间移动指针不唤出，
   * 否则翻页时工具栏一直在闪，反而碍事。
   *
   * 键盘（←/→/PageUp/PageDown/空格翻页、Ctrl+F 等）、滚轮翻页、点按都**不唤出**：
   * 这些操作一次接一次，若也 pokeChrome，工具栏会跟着每页闪一下。
   * 抽屉的开与关另有 pokeChrome，保证抽屉期间入口可见，不受这里的限制。
   */
  function onPointerMove(e: PointerEvent): void {
    pointerY = e.clientY;
    updateSideHints(e);
    if (e.clientY <= EDGE_BAND || e.clientY >= window.innerHeight - EDGE_BAND) pokeChrome();
  }

  /**
   * 左右热区的翻页箭头。只有「点击翻页」开着、指针确实在正文上、没有抽屉时
   * 才显示；从左右边缘滑出，不拦点击。
   */
  function updateSideHints(e: PointerEvent): void {
    const on = prefs.clickToTurn && !root.dataset.drawer;
    // 指针移到按钮本身时也要保持显示，否则按钮会在鼠标底下消失
    const inPage =
      e.target instanceof Element && !!e.target.closest(".page-viewport, .page-hint");
    const w = window.innerWidth;
    leftHint.classList.toggle("is-on", on && inPage && e.clientX < w * CLICK_SIDE_RATIO);
    rightHint.classList.toggle("is-on", on && inPage && e.clientX > w * (1 - CLICK_SIDE_RATIO));
  }

  /** 指针离开窗口时把提示收掉，否则会停在边缘 */
  function hideSideHints(): void {
    leftHint.classList.remove("is-on");
    rightHint.classList.remove("is-on");
  }

  /** 点按 / 滚轮只更新指针位置，供「指针压在工具栏上时不收起」判断，不负责唤出 */
  function trackPointerY(e: { clientY: number }): void {
    pointerY = e.clientY;
  }

  document.addEventListener("pointermove", onPointerMove, { passive: true, signal: ac.signal });
  document.addEventListener("pointerdown", trackPointerY, { passive: true, signal: ac.signal });
  document.addEventListener("wheel", trackPointerY, { passive: true, signal: ac.signal });
  window.addEventListener("blur", hideSideHints, { signal: ac.signal });

  // 指针离开工具栏后重新开始计时
  for (const bar of [topBar, bottomBar]) {
    bar.addEventListener("pointerleave", () => pokeChrome(), { signal: ac.signal });
  }

  pokeChrome();

  // ------------------------------------------------------------ 生命周期

  /**
   * 窗口尺寸变了：边距上限是按视口宽反推的，得同步滑块和 CSS，再按锚点重排。
   * 上限没变时（比如用户正在拖边距滑块）只重排，不回写滑块，免得打断手势。
   */
  function onViewportResize(): void {
    const cap = usableMargin();
    if (cap !== marginCap) {
      marginCap = cap;
      const eff = effectiveMargin();
      root.style.setProperty("--reader-margin-x", `${eff}px`);
      syncMarginUi();
    }
    repaginate();
  }

  /** 拖窗口时 RO 一帧可能触发多次，合并成一次重排（每次都要二分量字符锚点） */
  let resizeRaf = 0;
  const observer = new ResizeObserver(() => {
    if (resizeRaf) return;
    resizeRaf = requestAnimationFrame(() => {
      resizeRaf = 0;
      onViewportResize();
    });
  });
  observer.observe(paginator.el);

  function leave(): void {
    if (disposed) return;
    flushSaveNow();
    disposed = true;
    window.clearTimeout(saveTimer);
    window.clearTimeout(searchTimer);
    window.clearTimeout(searchFocusTimer);
    window.clearTimeout(idleTimer);
    cancelAnimationFrame(marginRaf);
    cancelAnimationFrame(resizeRaf);
    ac.abort();
    observer.disconnect();
    onExit();
  }

  /**
   * 上 / 左 = 上一页，下 / 右 = 下一页。方向键统一映射到整页切换，
   * 不做按行滚动（评估结论见 AGENTS.md「已决定不做」）。
   * 输入框里的 ↑↓ 由处理函数中的 `HTMLInputElement` 分支放行：搜索框留给结果导航，
   * 章节 / 边距滑杆留给原生步进，都不会误翻页。
   */
  const KEY_PREV_PAGE = new Set(["ArrowUp", "ArrowLeft", "PageUp"]);
  const KEY_NEXT_PAGE = new Set(["ArrowDown", "ArrowRight", "PageDown", " "]);

  document.addEventListener(
    "keydown",
    (e: KeyboardEvent) => {
      // 用应用内全文搜索替代 WebView2 的查找栏
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
        e.preventDefault();
        openDrawer("search");
        return;
      }
      const target = e.target instanceof Element ? e.target : null;
      // 输入框 / 下拉 / 抽屉里的按钮交给原生控件：以前这里只放行 HTMLInputElement，
      // select 的 ↑↓、按钮的空格都会被下面的 preventDefault 吃掉，键盘选不了系统字体。
      // 工具栏按钮不在放行范围内——否则点过一次翻页按钮后 ↑↓ 就再也不能翻页了。
      if (
        target?.closest("input, textarea, select, [contenteditable]") ||
        target?.closest(".drawer button")
      ) {
        if (e.key === "Escape") {
          (target as HTMLElement).blur();
          if (root.dataset.drawer) closeDrawers();
          else leave();
        }
        return;
      }
      if (KEY_NEXT_PAGE.has(e.key)) {
        e.preventDefault();
        turn(1);
      } else if (KEY_PREV_PAGE.has(e.key)) {
        e.preventDefault();
        turn(-1);
      } else if (e.key === "Escape") {
        if (root.dataset.drawer) closeDrawers();
        else leave();
      }
    },
    { signal: ac.signal },
  );

  let wheelAccum = 0;
  let wheelAt = 0;
  paginator.el.addEventListener(
    "wheel",
    (e: WheelEvent) => {
      const d = Math.abs(e.deltaY) >= Math.abs(e.deltaX) ? e.deltaY : e.deltaX;
      if (Math.abs(d) < 1) return;
      e.preventDefault();

      // deltaMode=1 是"行"，换算成近似像素；鼠标一格通常是 100px
      const px = e.deltaMode === 1 ? d * 16 : d;
      const now = performance.now();
      // 停顿或反向滚动都先清零：避免上一次的余量把这一次轻碰补成翻页
      if (now - wheelAt > WHEEL_RESET_MS || Math.sign(px) !== Math.sign(wheelAccum)) {
        wheelAccum = 0;
      }
      wheelAt = now;
      wheelAccum += px;

      // 一次事件最多翻一页；连续事件会凭借累加连续翻
      if (Math.abs(wheelAccum) < WHEEL_STEP) return;
      const dir = wheelAccum > 0 ? 1 : -1;
      wheelAccum -= dir * WHEEL_STEP;
      turn(dir);
    },
    { passive: false },
  );

  // ------------------------------------------------------------ 点击翻页

  /**
   * 点正文左右两侧翻页、点中间唤出工具栏。
   *
   * 翻页在 pointerup 里**立即**执行，不带任何延迟——220ms 的等待会被感知成
   * "点了没反应"。代价是左右热区里的双击选词会被第一下点击翻页吃掉
   * （中间 40% 不受影响）；拖选（位移 > 6px）在所有区域都正常。
   * 双击的第二下靠 `e.detail > 1` 跳过，不会一次翻两页。
   */
  paginator.el.addEventListener(
    "pointerdown",
    (e: PointerEvent) => {
      if (e.button !== 0) return;
      clickStart = { x: e.clientX, y: e.clientY, at: performance.now() };
    },
    { signal: ac.signal },
  );

  paginator.el.addEventListener(
    "pointerup",
    (e: PointerEvent) => {
      if (e.button !== 0 || !clickStart) return;
      const start = clickStart;
      clickStart = null;
      if (Math.abs(e.clientX - start.x) > 6 || Math.abs(e.clientY - start.y) > 6) return;
      if (performance.now() - start.at > CLICK_MAX_HOLD) return;
      // 双击的第二下：第一下已经翻过了，这里不再重复翻
      if (e.detail > 1) return;

      // 有选区说明刚拖选过，交给浏览器
      const sel = document.getSelection();
      if (sel && !sel.isCollapsed) return;

      const rect = paginator.el.getBoundingClientRect();
      const ratio = rect.width > 0 ? (e.clientX - rect.left) / rect.width : 0.5;

      if (ratio < CLICK_SIDE_RATIO) {
        if (prefs.clickToTurn) turn(-1);
      } else if (ratio > 1 - CLICK_SIDE_RATIO) {
        if (prefs.clickToTurn) turn(1);
      } else {
        // 中间只负责唤出工具栏；不在翻页键位上，避免每翻一页闪一次
        clearSearchHighlight();
        pokeChrome(CLICK_CHROME_HOLD);
      }
    },
    { signal: ac.signal },
  );

  // 正文右键：有选中文字才弹「复制」。原生菜单已在 app 层关掉，
  // 这是正文里唯一的右键复制入口（不选字就不弹，避免空菜单）。
  paginator.el.addEventListener(
    "contextmenu",
    (e: MouseEvent) => {
      const sel = document.getSelection();
      // 只认正文里的选区：在目录 / 搜索抽屉里选中的字不该在正文上弹「复制」
      const inBody = sel?.anchorNode ? paginator.el.contains(sel.anchorNode) : false;
      const text = inBody && sel && !sel.isCollapsed ? sel.toString() : "";
      if (!text.trim()) return;
      e.preventDefault();
      showContextMenu(e.clientX, e.clientY, [
        {
          label: "复制",
          onClick: () => {
            // 个别机器上 clipboard API 会被权限拦住，退回选区复制
            void navigator.clipboard.writeText(text).catch(() => document.execCommand("copy"));
          },
        },
      ]);
    },
    { signal: ac.signal },
  );

  // ------------------------------------------------------------ 加载

  async function boot(): Promise<void> {
    try {
      // 先把上次导入的本地字体装回去，再开章节——否则会先用内置字体排一遍版，
      // 字体到位后整页又要重排一次，视觉上会跳。
      if (prefs.fontPath && prefs.fontFamily === "custom") {
        try {
          await registerUserFont(prefs.fontPath);
        } catch (err) {
          console.warn("[reader] 本地字体加载失败，回落到内置字体", err);
          prefs.fontFamily = "serif";
          saveReaderPrefs(prefs);
          applyPrefs();
        }
      }

      chapters = await api.listChapters(bookId);
      if (chapters.length === 0) {
        setChapterLabel("这本书没有可读的章节");
        return;
      }

      chapterSlider.max = String(chapters.length - 1);
      applyTocFilter(tocFilter);

      const prog = await api.getProgress(bookId).catch(() => null);
      const start = Math.min(Math.max(prog?.chapterIndex ?? 0, 0), chapters.length - 1);
      await openChapter(start, prog?.charOffset ?? 0);
    } catch (err) {
      console.error("[reader] 打开失败", err);
      setChapterLabel("打开失败，文件可能已被移动");
    }
  }

  /** 左上角常驻章节名（工具栏自动隐藏时也保留） */
  function setChapterLabel(text: string): void {
    titleEl.textContent = text;
    titleEl.title = text;
  }

  /** 打开一章；失败返回 false，调用方据此决定要不要继续翻页 / 存进度 */
  async function openChapter(idx: number, charOffset = 0): Promise<boolean> {
    if (idx < 0 || idx >= chapters.length) return false;
    const seq = ++loadSeq;

    let content: ChapterContent;
    try {
      content = await api.readChapter(bookId, idx);
    } catch (err) {
      // 翻章失败不能静默：界面会停在上章，用户以为点击没生效
      if (disposed || seq !== loadSeq) return false;
      console.error("[reader] 打开章节失败", err);
      setChapterLabel("这一章读不出来，文件可能已被移动或改动");
      return false;
    }
    if (disposed || seq !== loadSeq) return false;

    chapterIndex = idx;
    setChapterLabel(content.title || bookTitle);

    paginator.load(content.text);
    await nextFrame();
    paginator.reflow();

    paginator.goToPage(charOffset > 0 ? paginator.pageOfChar(charOffset) : 0);
    updateChrome();
    renderTocWindow();
    return true;
  }

  function turn(delta: number): void {
    // 翻页即离开当前命中：高亮和结果行选中态一起收掉，别让黄块跟着翻
    clearSearchHighlight();
    const target = paginator.page + delta;

    if (target >= 0 && target < paginator.pageCount) {
      paginator.goToPage(target);
      updateChrome();
      scheduleSave();
      return;
    }

    if (delta > 0) {
      if (chapterIndex + 1 < chapters.length) {
        void openChapter(chapterIndex + 1, 0).then((ok) => { if (ok) scheduleSave(); });
      }
    } else if (chapterIndex > 0) {
      void openChapter(chapterIndex - 1, 0).then((ok) => {
        if (!ok) return;
        paginator.goToPage(paginator.pageCount - 1);
        updateChrome();
        scheduleSave();
      });
    }
  }

  function repaginate(): void {
    const anchor = paginator.charAtPageStart(paginator.page);
    paginator.reflow();
    paginator.goToPage(paginator.pageOfChar(anchor));
    updateChrome();
  }

  function updateChrome(): void {
    pageInfo.textContent = `第 ${paginator.page + 1} / ${paginator.pageCount} 页`;
    chapterInfo.textContent = `${chapterIndex + 1} / ${chapters.length} 章`;
    chapterSlider.value = String(chapterIndex);
    setSliderFill(chapterIndex);
    if (searchQuery && highlightActive && root.dataset.drawer !== "search") paintHighlight();
  }

  // ------------------------------------------------------------ 进度

  function scheduleSave(): void {
    window.clearTimeout(saveTimer);
    saveTimer = window.setTimeout(flushSaveNow, SAVE_DELAY);
  }

  function flushSaveNow(): void {
    if (chapters.length === 0) return;
    const charOffset = paginator.charAtPageStart(paginator.page);
    void api.saveProgress(bookId, chapterIndex, charOffset).catch(() => {
      // 存进度失败不该打断阅读
    });
  }

  // ------------------------------------------------------------ 目录

  /**
   * 按标题筛选目录。`next` 是输入框里去掉首尾空白后的关键词；
   * 只重算命中下标，章节本身不重新拉取。
   */
  function applyTocFilter(next: string): void {
    const wasEmpty = tocFilter === "";
    tocFilter = next;
    if (wasEmpty && next !== "") tocSavedScroll = tocScroll.scrollTop;

    if (tocFilter === "") {
      tocMatches = chapters.map((_, i) => i);
    } else {
      const q = tocFilter.toLowerCase();
      tocMatches = [];
      for (let i = 0; i < chapters.length; i++) {
        if (chapters[i].title.toLowerCase().includes(q)) tocMatches.push(i);
      }
    }

    // 先撑开高度再设置 scrollTop：否则恢复位置会被筛选后的旧高度钳到 0
    tocInner.style.height = `${tocMatches.length * TOC_ROW_H}px`;
    tocScroll.scrollTop = tocFilter === "" ? tocSavedScroll : 0;
    renderTocWindow();
  }

  /** 转义正则特殊字符：筛选词可能包含 ( ) . 等 */
  function escapeRegExp(text: string): string {
    return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  }

  /** 把 text 里所有命中 query 的片段包成指定 class（大小写不敏感） */
  function markMatches(text: string, query: string, className: string): DocumentFragment {
    const frag = document.createDocumentFragment();
    if (!query) {
      frag.appendChild(document.createTextNode(text));
      return frag;
    }

    const re = new RegExp(escapeRegExp(query), "gi");
    let from = 0;
    for (const match of text.matchAll(re)) {
      const at = match.index ?? 0;
      if (at > from) frag.appendChild(document.createTextNode(text.slice(from, at)));
      frag.appendChild(h("span", { class: className }, match[0]));
      from = at + match[0].length;
    }
    if (from < text.length) frag.appendChild(document.createTextNode(text.slice(from)));
    return frag;
  }

  /** 目录标题：筛选时把命中的关键词用 `.toc-match` 标出来 */
  function tocTitle(title: string): HTMLElement {
    const wrap = h("span", { class: "toc-title" });
    wrap.appendChild(markMatches(title, tocFilter, "toc-match"));
    return wrap;
  }

  function renderTocWindow(): void {
    if (chapters.length === 0) return;

    const total = tocMatches.length;
    clear(tocInner);
    if (total === 0) {
      tocInner.appendChild(h("div", { class: "toc-empty" }, "没有匹配的章节"));
      return;
    }

    const top = tocScroll.scrollTop;
    const viewH = tocScroll.clientHeight || 600;
    const first = Math.max(0, Math.floor(top / TOC_ROW_H) - 6);
    const last = Math.min(total, Math.ceil((top + viewH) / TOC_ROW_H) + 6);

    const frag = document.createDocumentFragment();
    for (let pos = first; pos < last; pos++) {
      const i = tocMatches[pos];
      const ch = chapters[i];
      frag.appendChild(
        h(
          "button",
          {
            class: `toc-row${i === chapterIndex ? " is-active" : ""}`,
            style: { top: `${pos * TOC_ROW_H}px`, height: `${TOC_ROW_H}px` },
            onClick: () => {
              // 打不开就留着目录，方便换一章；成功才收起抽屉
              clearSearchHighlight();
              void openChapter(i, 0).then((ok) => { if (ok) closeDrawers(); });
            },
          },
          h("span", { class: "toc-idx" }, String(i + 1)),
          tocTitle(ch.title),
        ),
      );
    }
    tocInner.appendChild(frag);
  }

  function scrollTocToActive(): void {
    if (tocMatches.length === 0) return;
    const pos = tocMatches.indexOf(chapterIndex);
    tocScroll.scrollTop = Math.max(
      0,
      (pos < 0 ? 0 : pos) * TOC_ROW_H - tocScroll.clientHeight / 2,
    );
    renderTocWindow();
  }

  // ------------------------------------------------------------ 搜索

  function searchMetaText(elapsedMs?: number): string {
    if (searchTotal === 0) return "没有找到";
    const shown = searchHits.length;
    // shown === total 时不再重复一次总数（原来会显示「命中 173 处 · 共 173 处」）
    const more = shown < searchTotal ? ` · 已显示 ${shown}/${searchTotal}` : "";
    const ms = elapsedMs === undefined ? "" : ` · ${elapsedMs.toFixed(1)} ms`;
    return `命中 ${searchTotal} 处${more}${ms}`;
  }

  /** 只在还有后续结果时显示「加载更多」 */
  function syncSearchMore(): void {
    searchMore.hidden = searchHits.length >= searchTotal;
    searchMore.disabled = searchLoadingMore;
    searchMore.textContent = searchLoadingMore
      ? "加载中…"
      : `加载更多（还有 ${Math.max(0, searchTotal - searchHits.length)} 处）`;
  }

  /** 渲染一批结果行，首次搜索与「加载更多」共用 */
  function appendHitRows(hits: SearchHit[]): void {
    const frag = document.createDocumentFragment();
    for (const hit of hits) {
      // 同一章的多条命中只在组头出现一次章节名，行内只留摘要
      if (hit.chapterIndex !== lastGroupChapter) {
        lastGroupChapter = hit.chapterIndex;
        frag.appendChild(h("div", { class: "hit-group" }, hit.chapterTitle));
      }

      const row = h(
        "button",
        {
          class: "hit-row",
          // 调试/验收用：能直接看出这条命中在书里的位置
          dataset: {
            chapter: String(hit.chapterIndex),
            offset: String(hit.charOffset),
          },
          onClick: () => {
            activeHit = hit;
            highlightActive = true;
            searchRowEls.forEach((r, j) =>
              r.classList.toggle("is-active", j === searchHits.indexOf(hit)),
            );
            // 不关抽屉：和 ↑↓ 保持一致，鼠标用户才能连续看同一条目的多处命中
            void openChapter(hit.chapterIndex, hit.charOffset).then((ok) => {
              if (ok) paintHighlight();
            });
          },
        },
        h("div", { class: "hit-snippet" }, markMatches(hit.snippet, searchQuery, "hit-match")),
      );
      searchRowEls.push(row);
      frag.appendChild(row);
    }
    searchList.appendChild(frag);
    if (activeHit) {
      const i = searchHits.indexOf(activeHit);
      searchRowEls.forEach((row, j) => row.classList.toggle("is-active", j === i));
    }
  }

  async function runSearch(): Promise<void> {
    const q = searchInput.value.trim();
    searchQuery = q;
    activeHit = null;
    highlightActive = false;
    searchHits = [];
    searchTotal = 0;
    searchRowEls = [];
    searchLoadingMore = false;
    lastGroupChapter = -1;
    clear(searchList);
    searchMore.hidden = true;
    setHighlight(null);

    if (q.length === 0) {
      searchMeta.textContent = "";
      return;
    }

    searchMeta.textContent = "搜索中…";
    try {
      const res = await api.search(bookId, q, SEARCH_PAGE, 0);
      if (q !== searchQuery) return; // 输入框又变了，丢弃这次响应
      searchHits = res.hits;
      searchTotal = res.total;
      appendHitRows(res.hits);
      searchMeta.textContent = searchMetaText(res.elapsedMs);
      syncSearchMore();
    } catch (err) {
      console.error(err);
      searchMeta.textContent = "搜索失败";
    }
  }

  /** 再拉一页命中，追加到现有结果后面（不清空、不重置选中态） */
  async function loadMoreHits(): Promise<void> {
    if (searchLoadingMore || searchHits.length >= searchTotal || !searchQuery) return;
    searchLoadingMore = true;
    syncSearchMore();
    const q = searchQuery;
    const offset = searchHits.length;
    try {
      const res = await api.search(bookId, q, SEARCH_PAGE, offset);
      if (q !== searchQuery) return;
      searchTotal = res.total;
      searchHits.push(...res.hits);
      appendHitRows(res.hits);
      searchMeta.textContent = searchMetaText();
    } catch (err) {
      console.error("[reader] 加载更多结果失败", err);
      searchMeta.textContent = "加载更多失败，点此重试";
    } finally {
      searchLoadingMore = false;
      if (q === searchQuery) syncSearchMore();
    }
  }

  /** 选中第 index 条命中：标记结果行，并让正文自动翻到命中页（搜索抽屉不关） */
  function selectSearchHit(index: number): void {
    if (searchHits.length === 0) return;
    const i = Math.min(Math.max(index, 0), searchHits.length - 1);
    const hit = searchHits[i];
    activeHit = hit;
    highlightActive = true;
    searchRowEls.forEach((row, j) => row.classList.toggle("is-active", j === i));
    searchRowEls[i]?.scrollIntoView({ block: "center" });
    void openChapter(hit.chapterIndex, hit.charOffset).then((ok) => {
      if (ok && activeHit === hit) paintHighlight();
    });
  }

  /**
   * 离开当前命中上下文（翻页 / 跳章 / 点正文中间）时收掉高亮，
   * 顺便清结果行的选中态。`searchQuery` 和结果列表都保留——
   * 重新打开搜索抽屉还能看到结果，再点一条又会重新高亮。
   */
  function clearSearchHighlight(): void {
    if (!searchQuery && !activeHit) return;
    highlightActive = false;
    activeHit = null;
    for (const row of searchRowEls) row.classList.remove("is-active");
    setHighlight(null);
  }

  /** 用 CSS Custom Highlight 画命中，不往正文里插节点，字符偏移不会被破坏 */
  function paintHighlight(): void {
    if (!highlightActive || !searchQuery || searchHits.length === 0) {
      setHighlight(null);
      return;
    }
    // 优先高亮用户点的那一条；翻到别的章之后再回退到本章第一条
    const hit =
      activeHit && activeHit.chapterIndex === chapterIndex
        ? activeHit
        : searchHits.find((x) => x.chapterIndex === chapterIndex);
    // 回退到本章第一条时，结果行的选中态要跟着走，否则正文高亮和列表对不上
    if (hit) {
      const i = searchHits.indexOf(hit);
      searchRowEls.forEach((row, j) => row.classList.toggle("is-active", j === i));
    }
    setHighlight(hit ? paginator.rangeOf(hit.charOffset, searchQuery.length) : null);
  }

  // ------------------------------------------------------------ 设置

  function buildSettings(): HTMLElement {
    const body = h("div", { class: "panel-body" });

    const sizeValue = h("span", { class: "setting-value" }, `${prefs.fontSize}px`);
    const sizeMinus = h(
      "button",
      {
        class: "step-btn",
        title: `最小 ${FONT_SIZE_MIN}px`,
        onClick: () => {
          prefs.fontSize = Math.max(FONT_SIZE_MIN, prefs.fontSize - 1);
          syncSizeUi();
          commitPrefs();
        },
      },
      "A−",
    );
    const sizePlus = h(
      "button",
      {
        class: "step-btn",
        title: `最大 ${FONT_SIZE_MAX}px`,
        onClick: () => {
          prefs.fontSize = Math.min(FONT_SIZE_MAX, prefs.fontSize + 1);
          syncSizeUi();
          commitPrefs();
        },
      },
      "A+",
    );
    /** 字号、边界禁用、显示值只在这里同步，恢复默认后也能复用 */
    function syncSizeUi(): void {
      sizeValue.textContent = `${prefs.fontSize}px`;
      sizeMinus.disabled = prefs.fontSize <= FONT_SIZE_MIN;
      sizePlus.disabled = prefs.fontSize >= FONT_SIZE_MAX;
    }

    const sizeRow = h(
      "div",
      { class: "setting-row" },
      h("span", { class: "setting-label" }, "字号"),
      h("div", { class: "setting-ctrl" }, sizeMinus, sizeValue, sizePlus),
    );

    // 行距：与字号同款 ± 步进（0.1 一档），下面再给四个常用档位一键跳转
    const lhValue = h("span", { class: "setting-value" }, `${prefs.lineHeight.toFixed(1)}×`);
    const lhMinus = h(
      "button",
      { class: "step-btn", title: "减小行距（0.1 一档）", onClick: () => stepLineHeight(-1) },
      "−",
    );
    const lhPlus = h(
      "button",
      { class: "step-btn", title: "增大行距（0.1 一档）", onClick: () => stepLineHeight(1) },
      "+",
    );
    const lhPresetButtons = LINE_HEIGHT_PRESETS.map((preset) =>
      h(
        "button",
        {
          class: "seg-btn",
          dataset: { v: String(preset.value) },
          title: `行距 ${preset.value.toFixed(1)}×`,
          onClick: () => setLineHeight(preset.value),
        },
        preset.label,
      ),
    );

    /** 数值、边界禁用、预设高亮只在这里同步，避免点完 ± 后旧预设还亮着 */
    function syncLineHeightUi(): void {
      lhValue.textContent = `${prefs.lineHeight.toFixed(1)}×`;
      lhMinus.disabled = prefs.lineHeight <= LINE_HEIGHT_MIN;
      lhPlus.disabled = prefs.lineHeight >= LINE_HEIGHT_MAX;
      for (const b of lhPresetButtons) {
        b.classList.toggle("is-active", Number(b.dataset.v) === prefs.lineHeight);
      }
    }

    function stepLineHeight(dir: number): void {
      // 以整数档位加减，不写 prefs.lineHeight + 0.1（浮点误差会一档档累积）
      const steps = Math.round(prefs.lineHeight / LINE_HEIGHT_STEP) + dir;
      setLineHeight(Number((steps * LINE_HEIGHT_STEP).toFixed(2)));
    }

    function setLineHeight(value: number): void {
      const next = clampLineHeight(value);
      if (next === prefs.lineHeight) return;
      prefs.lineHeight = next;
      syncLineHeightUi();
      commitPrefs();
    }
    syncLineHeightUi();

    const lhRow = h(
      "div",
      { class: "setting-row" },
      h("span", { class: "setting-label" }, "行距"),
      h("div", { class: "setting-ctrl" }, lhMinus, lhValue, lhPlus),
    );
    const lhPresetRow = h(
      "div",
      { class: "setting-row" },
      h("span", { class: "setting-label", title: "常用行距，一键切换" }, "预设"),
      h("div", { class: "setting-ctrl lh-presets" }, ...lhPresetButtons),
    );

    const FONT_LABELS: Record<ReaderFont, string> = {
      serif: "宋体",
      sans: "黑体",
      system: "系统",
      custom: "本地",
    };
    const fontButtons = (["serif", "sans", "system", "custom"] as const).map((f) =>
      h(
        "button",
        {
          class: `seg-btn${prefs.fontFamily === f ? " is-active" : ""}`,
          dataset: { f },
          title:
            f === "system"
              ? "使用电脑已安装的字体"
              : f === "custom"
                ? "使用下面导入的字体文件"
                : undefined,
          onClick: () => {
            if (f === "system") {
              void useSystemFont();
              return;
            }
            if (f === "custom") {
              void useCustomFont();
              return;
            }
            prefs.fontFamily = f;
            commitPrefs();
            syncFontUi();
          },
        },
        FONT_LABELS[f],
      ),
    );
    const fontRow = h(
      "div",
      { class: "setting-row" },
      h("span", { class: "setting-label" }, "字体"),
      h("div", { class: "setting-ctrl" }, ...fontButtons),
    );

    const systemSelect = h(
      "select",
      {
        class: "setting-select",
        title: "使用电脑已安装的字体，不需要导入文件",
        onChange: () => {
          const family = systemSelect.value;
          if (!family) return;
          prefs.systemFamily = family;
          prefs.fontFamily = "system";
          commitPrefs();
          syncFontUi();
        },
      },
      h("option", { value: "" }, "加载中…"),
    );
    const systemRow = h(
      "div",
      { class: "setting-row" },
      h("span", { class: "setting-label" }, "系统字体"),
      h("div", { class: "setting-ctrl" }, systemSelect),
    );

    void loadSystemFonts().then((fonts) => {
      if (fonts.length === 0) {
        systemSelect.replaceChildren(h("option", { value: "" }, "未找到系统字体"));
        systemSelect.disabled = true;
        return;
      }
      systemSelect.replaceChildren(
        h("option", { value: "" }, "选择系统字体…"),
        ...fonts.map((f) => h("option", { value: f }, f)),
      );
      if (prefs.systemFamily && fonts.includes(prefs.systemFamily)) {
        systemSelect.value = prefs.systemFamily;
      }
    });

    const fontFileValue = h("span", { class: "setting-value font-file" }, baseName(prefs.fontPath));
    const importBtn = h("button", { class: "text-btn", onClick: () => void importFont() }, "导入…");
    const removeBtn = h("button", { class: "text-btn", onClick: () => removeFont() }, "移除");
    const fontFileRow = h(
      "div",
      { class: "setting-row" },
      h("span", { class: "setting-label" }, "字体文件"),
      h("div", { class: "setting-ctrl" }, fontFileValue, importBtn, removeBtn),
    );

    function syncFontUi(): void {
      fontFileValue.textContent = baseName(prefs.fontPath);
      fontFileValue.title = prefs.fontPath ?? "";
      importBtn.textContent = prefs.fontPath ? "更换…" : "导入…";
      removeBtn.disabled = !prefs.fontPath;
      // 恢复默认会把 systemFamily 清空，下拉框要跟着回到占位项
      if (!prefs.systemFamily) systemSelect.value = "";

      const customBtn = fontButtons.find((b) => b.dataset.f === "custom");
      if (customBtn) customBtn.disabled = !prefs.fontPath;
      for (const b of fontButtons) {
        b.classList.toggle("is-active", b.dataset.f === prefs.fontFamily);
      }
    }

    /** 从内置字体切回本地字体时，字体可能因为之前没用而没注册过——按需补装 */
    async function useCustomFont(): Promise<void> {
      const path = prefs.fontPath;
      if (!path) return;
      try {
        await registerUserFont(path);
      } catch (err) {
        fontFileValue.textContent = "字体不可用";
        fontFileValue.title = String(err);
        console.error("[reader] 本地字体加载失败", err);
        return;
      }
      prefs.fontFamily = "custom";
      commitPrefs();
      syncFontUi();
    }

    /** 直接使用系统已安装的字体；还没选过就优先挑微软雅黑 */
    async function useSystemFont(): Promise<void> {
      const fonts = await loadSystemFonts();
      if (fonts.length === 0) {
        systemSelect.disabled = true;
        systemSelect.title = "没有读到系统字体";
        return;
      }
      if (!prefs.systemFamily || !fonts.includes(prefs.systemFamily)) {
        prefs.systemFamily = fonts.includes("Microsoft YaHei") ? "Microsoft YaHei" : fonts[0];
        systemSelect.value = prefs.systemFamily;
      }
      prefs.fontFamily = "system";
      commitPrefs();
      syncFontUi();
    }

    async function importFont(): Promise<void> {
      if (!isTauri()) {
        fontFileValue.textContent = "需在客户端中使用";
        return;
      }
      try {
        const path = await pickFontFile();
        if (!path) return;

        // 读盘 + 解析可能几百毫秒，先给个反馈，别让人以为点了没反应
        fontFileValue.textContent = "加载中…";
        await registerUserFont(path);

        prefs.fontPath = path;
        prefs.fontFamily = "custom";
        commitPrefs();
        syncFontUi();
      } catch (err) {
        fontFileValue.textContent = "导入失败";
        fontFileValue.title = String(err);
        console.error("[reader] 字体导入失败", err);
      }
    }

    function removeFont(): void {
      unregisterUserFont();
      prefs.fontPath = null;
      if (prefs.fontFamily === "custom") prefs.fontFamily = "serif";
      commitPrefs();
      syncFontUi();
    }

    syncFontUi();

    const themeButtons = (Object.keys(THEME_LABELS) as ReaderTheme[]).map((t) =>
      h(
        "button",
        {
          class: `theme-dot${prefs.theme === t ? " is-active" : ""}`,
          dataset: { theme: t },
          onClick: () => {
            prefs.theme = t;
            commitPrefs();
            for (const b of themeButtons) b.classList.toggle("is-active", b.dataset.theme === t);
          },
        },
        THEME_LABELS[t],
      ),
    );
    const themeRow = h(
      "div",
      { class: "setting-row" },
      h("span", { class: "setting-label" }, "主题"),
      h("div", { class: "setting-ctrl" }, ...themeButtons),
    );

    const marginNumber = h("input", {
      class: "margin-number",
      type: "number",
      min: String(MARGIN_MIN),
      max: String(usableMargin()),
      step: "1",
      value: String(effectiveMargin()),
      "aria-label": "左右边距（像素）",
      // 1px 精度靠它：120px 宽的滑块每像素要跨好几像素，只做粗调
      onChange: () => applyMarginFromNumber(),
      onKeyDown: (e: KeyboardEvent) => {
        if (e.key === "Enter") {
          e.preventDefault();
          applyMarginFromNumber();
        }
      },
    });
    marginNumberEl = marginNumber;

    const marginInput = h("input", {
      class: "margin-slider",
      type: "range",
      min: String(MARGIN_MIN),
      // 上限随窗口宽度走：1280 宽时约 580px，能把正文挤成一条窄竖列
      max: String(usableMargin()),
      step: String(MARGIN_STEP),
      value: String(effectiveMargin()),
      onInput: (e: Event) => {
        const v = Number((e.target as HTMLInputElement).value);
        if (!Number.isFinite(v)) return;
        applyMarginLive(v);
      },
      // 松手才落库并做最后一次重排；拖动过程中只按帧合并重排
      onChange: () => {
        settleMargin();
        marginAnchor = null;
        saveReaderPrefs(prefs);
      },
    });
    marginSlider = marginInput;
    const marginRow = h(
      "div",
      { class: "setting-row" },
      h("span", { class: "setting-label" }, "边距"),
      h(
        "div",
        { class: "setting-ctrl margin-ctrl" },
        marginInput,
        h(
          "span",
          { class: "margin-number-wrap" },
          marginNumber,
          h("span", { class: "margin-unit" }, "px"),
        ),
      ),
    );

    // 点击翻页开关：只影响左右热区，中间唤出工具栏不受它控制
    const clickButtons = [true, false].map((on) =>
      h(
        "button",
        {
          class: `seg-btn${prefs.clickToTurn === on ? " is-active" : ""}`,
          dataset: { on: String(on) },
          onClick: () => {
            prefs.clickToTurn = on;
            syncClickUi();
            if (!on) hideSideHints();
            // 不触发重排：点击翻页只改交互，不动版式
            saveReaderPrefs(prefs);
          },
        },
        on ? "开" : "关",
      ),
    );
    function syncClickUi(): void {
      for (const b of clickButtons) {
        b.classList.toggle("is-active", b.dataset.on === String(prefs.clickToTurn));
      }
    }
    const clickRow = h(
      "div",
      { class: "setting-row" },
      h("span", { class: "setting-label" }, "点击翻页"),
      h("div", { class: "setting-ctrl" }, ...clickButtons),
    );

    const resetBtn = h(
      "button",
      { class: "btn-ghost setting-reset", type: "button", onClick: () => resetPrefs() },
      "恢复默认",
    );
    const resetBlock = h(
      "div",
      { class: "setting-reset-block" },
      resetBtn,
      h("p", { class: "setting-hint" }, "字号、行距、字体、主题和边距回到初始值；导入的字体文件保留。"),
    );

    function group(title: string, ...rows: HTMLElement[]): HTMLElement {
      return h(
        "section",
        { class: "setting-group" },
        h("h3", { class: "setting-group-title" }, title),
        ...rows,
      );
    }

    /** 所有控件 ← prefs 的单向同步；恢复默认后一次性刷新 */
    function syncPrefsUi(): void {
      syncSizeUi();
      syncLineHeightUi();
      syncFontUi();
      syncClickUi();
      syncMarginUi();
      for (const b of themeButtons) {
        b.classList.toggle("is-active", b.dataset.theme === prefs.theme);
      }
    }

    function resetPrefs(): void {
      const anchor = paginator.charAtPageStart(paginator.page);
      // 字体文件是资源不是样式：保留路径，其余排版项回默认
      const fontPath = prefs.fontPath;
      Object.assign(prefs, { ...DEFAULT_PREFS, fontPath });
      prefs.lineHeight = clampLineHeight(DEFAULT_PREFS.lineHeight);
      saveReaderPrefs(prefs);
      syncPrefsUi();
      applyPrefs();
      appliedFontKey = fontKey();
      paginator.reflow();
      paginator.goToPage(paginator.pageOfChar(anchor));
      updateChrome();
    }

    syncPrefsUi();

    body.append(
      group("排版", sizeRow, lhRow, lhPresetRow, marginRow, clickRow),
      group("字体", fontRow, systemRow, fontFileRow),
      group("外观", themeRow),
      resetBlock,
    );
    return body;
  }

  /** 上一次生效的字体组合；只有它变了才需要等 Web 字体加载完再重排 */
  let appliedFontKey = "";
  function fontKey(): string {
    return `${prefs.fontFamily}|${prefs.systemFamily}|${prefs.fontPath ?? ""}`;
  }

  function commitPrefs(): void {
    // 锚点必须在改 CSS 之前量：applyPrefs 之后布局已经脏了，再用旧的 colWidth
    // 去算栏号会偏页，表现就是改完字号阅读位置跳一下。
    const anchor = paginator.charAtPageStart(paginator.page);
    saveReaderPrefs(prefs);
    const key = fontKey();
    const fontChanged = key !== appliedFontKey;
    appliedFontKey = key;
    applyPrefs();
    paginator.reflow();
    paginator.goToPage(paginator.pageOfChar(anchor));
    updateChrome();
    if (fontChanged) {
      // 刚切字体时浏览器可能还在用回落字体排版，pageCount 会算错，
      // 恢复进度 / 搜索跳转可能偏一页。字体落定后按同一个锚点再排一次。
      void document.fonts.ready.then(() => {
        if (disposed) return;
        paginator.reflow();
        paginator.goToPage(paginator.pageOfChar(anchor));
        updateChrome();
      });
    }
  }

  /**
   * 边距滑杆专用：CSS 变量立刻生效（视觉实时），重排按帧合并。
   * 每个 input 事件都走一次 commitPrefs 的话，拖一次要重排几十遍。
   */
  function applyMarginLive(v: number): void {
    // 第一次 input 时 CSS 还没变，此刻量到的才是旧版式下的锚点
    if (marginAnchor === null) marginAnchor = paginator.charAtPageStart(paginator.page);
    prefs.marginX = v;
    if (marginNumberEl) marginNumberEl.value = String(Math.min(v, usableMargin()));
    root.style.setProperty("--reader-margin-x", `${Math.min(v, usableMargin())}px`);
    if (marginRaf) return;
    marginRaf = requestAnimationFrame(() => {
      marginRaf = 0;
      settleMargin();
    });
  }

  /** 边距滑块 / 数字框 / 实际渲染值三者对齐 */
  function syncMarginUi(): void {
    const cap = usableMargin();
    const eff = effectiveMargin();
    if (marginSlider) {
      marginSlider.max = String(cap);
      marginSlider.value = String(eff);
    }
    if (marginNumberEl) {
      marginNumberEl.max = String(cap);
      marginNumberEl.value = String(eff);
    }
  }

  /** 数字框提交：1px 精度，按锚点重排后立刻落库 */
  function applyMarginFromNumber(): void {
    const el = marginNumberEl;
    if (!el) return;
    const raw = Number(el.value);
    if (!Number.isFinite(raw)) {
      syncMarginUi();
      return;
    }
    const next = Math.min(usableMargin(), Math.max(MARGIN_MIN, Math.round(raw)));
    if (next === effectiveMargin() && next === prefs.marginX) {
      syncMarginUi();
      return;
    }

    const anchor = paginator.charAtPageStart(paginator.page);
    prefs.marginX = next;
    root.style.setProperty("--reader-margin-x", `${next}px`);
    paginator.reflow();
    paginator.goToPage(paginator.pageOfChar(anchor));
    updateChrome();
    syncMarginUi();
    saveReaderPrefs(prefs);
  }

  /** 按拖动开始时的字符锚点重排一次；不落库（松手时才写） */
  function settleMargin(): void {
    if (marginRaf) {
      cancelAnimationFrame(marginRaf);
      marginRaf = 0;
    }
    const anchor = marginAnchor;
    paginator.reflow();
    if (anchor !== null) paginator.goToPage(paginator.pageOfChar(anchor));
    updateChrome();
    if (marginNumberEl) marginNumberEl.value = String(effectiveMargin());
  }

  /** 当前窗口允许的最大边距：按正文最小宽度反推，向下对齐到步长 */
  function usableMargin(): number {
    // 还没挂到 DOM 时 clientWidth 是 0，用窗口宽兜底（阅读页本来就占满窗口）
    const w = paginator.el.clientWidth || window.innerWidth;
    const cap = Math.max(0, Math.floor((w - MIN_CONTENT_W) / 2));
    return Math.max(MARGIN_MIN, Math.floor(cap / MARGIN_STEP) * MARGIN_STEP);
  }

  /**
   * 存的偏好可能超过当前窗口能承受的上限（用户在大窗口设的），渲染时钳一下。
   * 不改写 `prefs.marginX`：窗口拉回来后原来的偏好还在。
   */
  function effectiveMargin(): number {
    return Math.min(prefs.marginX, usableMargin());
  }

  function applyPrefs(): void {
    root.dataset.theme = prefs.theme;
    root.style.setProperty("--reader-font-size", `${prefs.fontSize}px`);
    root.style.setProperty("--reader-line-height", String(prefs.lineHeight));
    root.style.setProperty("--reader-font", fontStack());
    root.style.setProperty("--reader-margin-x", `${effectiveMargin()}px`);
  }

  /**
   * 自定义字体在前、内置字体兜底：字体没加载好或字形缺失时，
   * 浏览器会逐字回落，不会出现整页空白。
   */
  function fontStack(): string {
    if (prefs.fontFamily === "custom") return `"${USER_FONT_FAMILY}", var(--font-serif)`;
    if (prefs.fontFamily === "system" && prefs.systemFamily) {
      const family = prefs.systemFamily.replace(/["\\]/g, "");
      return `"${family}", var(--font-serif)`;
    }
    return prefs.fontFamily === "sans" ? "var(--font-sans)" : "var(--font-serif)";
  }

  // ------------------------------------------------------------ 抽屉开关

  function openDrawer(kind: DrawerKind): void {
    const title = kind === "toc" ? `目录 · ${bookTitle}` : DRAWER_TITLES[kind];
    drawerTitle.textContent = title;
    drawerTitle.title = title;
    drawerBody.replaceChildren(panels[kind]);

    const left = kind === "toc";
    const atSide = drawerEl.classList.contains("is-left") === left;
    if (!atSide && !root.dataset.drawer) {
      // 关着时换边要先无动画地放到目标侧的屏幕外，否则 transform 过渡
      // 会从对侧的位移开始，看起来像从中间平移过来。
      drawerEl.style.transition = "none";
      drawerEl.classList.toggle("is-left", left);
      drawerEl.classList.toggle("is-right", !left);
      void drawerEl.offsetWidth;
      drawerEl.style.transition = "";
    } else {
      drawerEl.classList.toggle("is-left", left);
      drawerEl.classList.toggle("is-right", !left);
    }

    root.dataset.drawer = kind;
    hideSideHints();
    pokeChrome(); // 抽屉开着时不隐藏工具栏，否则关掉抽屉会找不到入口

    if (kind === "toc") scrollTocToActive();
    if (kind === "search") {
      // 必须 preventScroll：此刻抽屉还在 240ms 的滑入动画中，搜索框右半截仍在视口外，
      // 普通 focus() 会把 .reader（overflow:hidden 但**依然可被程序化滚动**）横向滚过去，
      // 正文跟着左移；动画结束后溢出区缩小、scrollLeft 又被钳回 0，看起来就是"左右闪一下"。
      window.clearTimeout(searchFocusTimer);
      searchFocusTimer = window.setTimeout(() => searchInput.focus({ preventScroll: true }), 80);
    }
  }

  function closeDrawers(): void {
    // 定时器不清的话，快速「开搜索 → 关 → 开目录」会把焦点丢给已移出 DOM 的 input，
    // ↑↓ 结果导航随之失效。
    window.clearTimeout(searchFocusTimer);
    searchFocusTimer = 0;
    delete root.dataset.drawer;
    pokeChrome();
  }

  return root;
}

function nextFrame(): Promise<void> {
  return new Promise((resolve) => requestAnimationFrame(() => resolve()));
}

/** 只留文件名，完整路径放 title 里——设置面板一行放不下整条路径 */
function baseName(path: string | null): string {
  if (!path) return "未导入";
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

function setHighlight(range: Range | null): void {
  const w = window as unknown as {
    Highlight?: new (...r: Range[]) => unknown;
    CSS?: { highlights?: Map<string, unknown> };
  };
  const store = w.CSS?.highlights;
  if (!store) return;
  if (!range || !w.Highlight) {
    store.delete("search-hit");
    return;
  }
  store.set("search-hit", new w.Highlight(range));
}
