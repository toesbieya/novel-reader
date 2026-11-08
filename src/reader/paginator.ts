/**
 * 分页器 —— 整个阅读体验的地基。
 *
 * 做法：把整章文字塞进一个"固定高度 + 多栏"的容器里，让浏览器按栏排版，
 * 每栏就是一页。翻页只改外层 `translate3d`，不重排、不改 DOM，
 * 因此由合成器完成，一帧内就能刷完。
 *
 * 为什么不自己算断行：中文禁则、标点挤压、字体回退这些规则，浏览器比我们写得好，
 * 而且 C++ 实现比 JS 快一个数量级。
 *
 * 关键实现细节：
 * - `column-fill: auto` 是必须的，否则浏览器会把内容均摊到各栏而不是顺序填充
 * - 测量用 `getBoundingClientRect()` 的**相对差值**，这样即使祖先元素带
 *   translate 变换，算出来的栏号依然正确（绝对坐标会跟着一起平移）
 */

import { h } from "../core/dom";

/** 栏间留白。视觉上正好是两页之间的分隔，不能为 0，否则相邻页的字会粘在一起。 */
const COLUMN_GAP = 64;
/** 栏宽窄于此值就切到「窄竖列」排版：每行只剩几个字，justify 会把字距拉得很散。 */
const NARROW_COL_W = 240;

export class Paginator {
  readonly el: HTMLElement;

  private clip: HTMLElement;
  private track: HTMLElement;
  private cols: HTMLElement;
  /** 二分测量复用的临时 Range：charAtPageStart 一次要量十几到二十次 */
  private measureRange = document.createRange();
  /** 原始段落（保留 \r 等不可见字符，保证字符偏移和引擎一致） */
  private paras: string[] = [];
  /** 每段首个字符在整章中的偏移 */
  private starts: number[] = [];
  private totalChars = 0;
  private colWidth = 0;

  private current = 0;
  pageCount = 1;

  constructor() {
    this.cols = h("div", { class: "page-columns" });
    this.track = h("div", { class: "page-track" }, this.cols);
    // 裁剪必须在这一层：viewport 带 padding，overflow 是按 padding box 算的，
    // 直接裁 viewport 会把左右留白区也放进来，下一页的字就会从边上漏出来。
    this.clip = h("div", { class: "page-clip" }, this.track);
    this.el = h("div", { class: "page-viewport" }, this.clip);
  }

  get page(): number {
    return this.current;
  }

  get chars(): number {
    return this.totalChars;
  }

  /** 换一章内容。不在这里排版——尺寸可能还没稳定，交给 reflow()。 */
  load(text: string): void {
    this.paras = [];
    this.starts = [];
    this.totalChars = 0;

    const frag = document.createDocumentFragment();
    let pos = 0;

    for (const raw of text.split("\n")) {
      // 空行要占一个视觉行高，否则段落之间会挤在一起；
      // 但字符偏移上它贡献 0，不能塞 nbsp 污染偏移计算
      const p = h("p", raw.length === 0 ? { class: "is-blank" } : null);
      p.appendChild(document.createTextNode(raw));
      frag.appendChild(p);

      this.paras.push(raw);
      this.starts.push(pos);
      pos += raw.length + 1; // +1 是被 split 吃掉的 '\n'
    }

    this.totalChars = Math.max(0, pos - 1);
    this.cols.replaceChildren(frag);
    this.current = 0;
    this.pageCount = 1;
    this.applyTransform();
  }

  /** 按当前容器尺寸重新分栏。字号、行距、窗口变化后都要调。 */
  reflow(): void {
    const w = this.track.clientWidth;
    const ht = this.track.clientHeight;
    if (w <= 0 || ht <= 0) return;

    this.cols.style.height = `${ht}px`;
    this.cols.style.columnWidth = `${w}px`;
    this.cols.style.columnGap = `${COLUMN_GAP}px`;
    this.cols.classList.toggle("is-narrow", w < NARROW_COL_W);
    this.colWidth = w;

    // N 栏时 scrollWidth = N*w + (N-1)*gap，反解出 N
    const sw = this.cols.scrollWidth;
    this.pageCount = Math.max(1, Math.round((sw + COLUMN_GAP) / (w + COLUMN_GAP)));

    if (this.current > this.pageCount - 1) this.current = this.pageCount - 1;
    this.applyTransform();
  }

  goToPage(page: number): void {
    const p = Math.max(0, Math.min(page, this.pageCount - 1));
    if (p === this.current) return;
    this.current = p;
    this.applyTransform();
  }

  /** 指定字符偏移落在第几页 */
  pageOfChar(offset: number): number {
    return this.columnOf(offset);
  }

  /** 第 page 页第一个字符的偏移，用于存进度 */
  charAtPageStart(page: number): number {
    if (page <= 0) return 0;
    // 同一次二分里 track 的 transform 不变，基准坐标只量一次就够了。
    // 以前每轮 columnOf 都 createRange + 两次 getBoundingClientRect，
    // 改一次字号/行距就是十几倍强制布局；现在 Range 复用、基准复用。
    const base = this.cols.getBoundingClientRect().left;
    let lo = 0;
    let hi = this.totalChars;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (this.columnOf(mid, base) >= page) hi = mid;
      else lo = mid + 1;
    }
    return lo;
  }

  private applyTransform(): void {
    const step = this.colWidth + COLUMN_GAP;
    this.track.style.transform = `translate3d(${-this.current * step}px, 0, 0)`;
  }

  /**
   * 字符偏移 → 栏号。
   * 用 Range 量单个字符的位置，相对容器左边除以「栏宽 + 栏间距」后向下取整。
   * 不能用 round：字符在页面右半边时会被算到下一页（搜索跳转会多翻一页）。
   */
  private columnOf(offset: number, base = this.cols.getBoundingClientRect().left): number {
    const hit = this.locate(offset);
    if (!hit) return 0;

    const step = this.colWidth + COLUMN_GAP;

    if (hit.node && hit.node.length > 0) {
      // 落在段落末尾时（偏移指向换行符）退回量最后一个字
      const inner = Math.min(hit.inner, hit.node.length - 1);
      const range = this.measureRange;
      range.setStart(hit.node, inner);
      range.setEnd(hit.node, inner + 1);
      const r = range.getBoundingClientRect();
      if (r.width > 0 || r.height > 0) {
        return Math.max(0, Math.floor((r.left - base) / step));
      }
    }

    // 空段落：退化成量段落元素本身的位置
    const er = hit.el.getBoundingClientRect();
    return Math.max(0, Math.floor((er.left - base) / step));
  }

  /** 定位字符偏移所在的段落与段内位置 */
  private locate(
    offset: number,
  ): { el: HTMLElement; node: Text | null; inner: number; idx: number } | null {
    if (this.paras.length === 0) return null;

    const o = Math.max(0, Math.min(offset, this.totalChars));

    // 二分找到所属段落
    let lo = 0;
    let hi = this.paras.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (this.starts[mid] <= o) lo = mid;
      else hi = mid - 1;
    }

    const el = this.cols.children[lo] as HTMLElement | undefined;
    if (!el) return null;

    const first = el.firstChild;
    const node = first && first.nodeType === Node.TEXT_NODE ? (first as Text) : null;
    const inner = node ? Math.max(0, Math.min(o - this.starts[lo], node.length)) : 0;

    return { el, node, inner, idx: lo };
  }

  /**
   * 生成一段 DOM Range，供 CSS Custom Highlight API 画搜索命中。
   * 命中落在段落末尾 / 跨到下一段时，旧实现 `end<=start` 直接返回 null，
   * 表现为「搜得到、点过去却没高亮」。这里尽量从下一段开头补画，
   * 已经到章末就高亮最后一个字，至少给出可见落点。
   */
  rangeOf(offset: number, length: number): Range | null {
    const hit = this.locate(offset);
    if (!hit) return null;
    // 这里不能复用 measureRange：CSS Highlight 会长期持有这个 Range 对象
    const range = document.createRange();

    if (hit.node && hit.inner < hit.node.length) {
      const start = hit.inner;
      range.setStart(hit.node, start);
      range.setEnd(hit.node, Math.min(start + length, hit.node.length));
      return range;
    }

    const next = this.cols.children[hit.idx + 1] as HTMLElement | undefined;
    const nextNode = next?.firstChild;
    if (nextNode && nextNode.nodeType === Node.TEXT_NODE) {
      const t = nextNode as Text;
      if (t.length > 0) {
        range.setStart(t, 0);
        range.setEnd(t, Math.min(length, t.length));
        return range;
      }
    }

    // 章末、或下一段是空行：高亮当前段最后一个字，别让点击结果毫无反应
    if (hit.node && hit.node.length > 0) {
      range.setStart(hit.node, hit.node.length - 1);
      range.setEnd(hit.node, hit.node.length);
      return range;
    }
    return null;
  }
}
