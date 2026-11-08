/**
 * 指针拖拽。
 *
 * 为什么不用 HTML5 `draggable`：Tauri 的 `dragDropEnabled: true` 会在 Windows 上
 * 接管整个 WebView 的拖放，HTML5 DnD 事件根本不会触发。而关掉它就会丢掉
 * "把 txt 拖进窗口导入"——那个更值钱。所以这里用 pointer 事件手写一套。
 *
 * 用法：在 pointerdown 里调用 startDrag，模块接管后续 move/up，
 * 只在指针真的移动超过阈值后才进入拖拽态，不影响普通点击。
 */

import { h } from "./dom";

export type DropZone = "top" | "middle" | "bottom";

export interface DragTarget {
  el: Element;
  /** 落在目标的上/中/下三段，用来区分"插到前面"和"放进去" */
  zone: DropZone;
}

export interface DragOptions {
  /** 触发拖拽的 pointerdown 事件 */
  start: PointerEvent;
  /** 跟随光标的小标签文案 */
  label: string;
  /** 投放目标选择器 */
  selector: string;
  /** 目标变化时回调，传 null 表示当前没有目标（记得在这里清高亮） */
  onHover?: (next: DragTarget | null, prev: Element | null) => void;
  onDrop: (target: DragTarget) => void;
}

/** 指针移动超过这个距离才算拖拽，否则当成点击 */
const THRESHOLD = 5;

function zoneOf(el: Element, y: number): DropZone {
  const r = el.getBoundingClientRect();
  const t = r.height > 0 ? (y - r.top) / r.height : 0.5;
  return t < 0.3 ? "top" : t > 0.7 ? "bottom" : "middle";
}

/**
 * 拖完会紧跟一个 click（在原来的元素上），必须吃掉它——
 * 否则拖书=顺便把书打开了，拖目录=顺便切换了目录。
 */
function swallowNextClick(): void {
  const stop = (e: MouseEvent): void => {
    e.preventDefault();
    e.stopPropagation();
    window.removeEventListener("click", stop, true);
  };
  window.addEventListener("click", stop, true);
  window.setTimeout(() => window.removeEventListener("click", stop, true), 250);
}

export function startDrag(opts: DragOptions): void {
  if (opts.start.button !== 0) return;

  const sx = opts.start.clientX;
  const sy = opts.start.clientY;

  let dragging = false;
  let ghost: HTMLElement | null = null;
  let lastEl: Element | null = null;
  let lastZone: DropZone = "middle";

  const targetAt = (x: number, y: number): DragTarget | null => {
    // 幽灵层是 pointer-events:none，不会挡住命中测试
    const el = document.elementFromPoint(x, y)?.closest(opts.selector) ?? null;
    return el ? { el, zone: zoneOf(el, y) } : null;
  };

  const detach = (): void => {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
    window.removeEventListener("pointercancel", onCancel);
    document.removeEventListener("keydown", onKey, true);
    ghost?.remove();
    ghost = null;
    document.body.classList.remove("is-dragging");
  };

  const endWith = (target: DragTarget | null): void => {
    const hadFocus = dragging;
    detach();
    opts.onHover?.(null, lastEl);
    lastEl = null;
    if (!hadFocus) return;
    if (target) opts.onDrop(target);
  };

  function onMove(e: PointerEvent): void {
    if (!dragging) {
      if (Math.hypot(e.clientX - sx, e.clientY - sy) < THRESHOLD) return;
      dragging = true;
      document.body.classList.add("is-dragging");
      ghost = h("div", { class: "drag-ghost" }, opts.label);
      document.body.appendChild(ghost);
    }

    e.preventDefault();
    if (ghost) {
      ghost.style.left = `${e.clientX + 14}px`;
      ghost.style.top = `${e.clientY + 12}px`;
    }

    const next = targetAt(e.clientX, e.clientY);
    const nextEl = next?.el ?? null;
    if (nextEl !== lastEl || (next && next.zone !== lastZone)) {
      opts.onHover?.(next, lastEl);
      lastEl = nextEl;
      lastZone = next?.zone ?? "middle";
    }
  }

  function onUp(e: PointerEvent): void {
    if (e.button !== 0) return;
    endWith(dragging ? targetAt(e.clientX, e.clientY) : null);
    if (dragging) swallowNextClick();
  }

  function onCancel(): void {
    endWith(null);
  }

  function onKey(e: KeyboardEvent): void {
    if (e.key === "Escape") endWith(null);
  }

  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
  window.addEventListener("pointercancel", onCancel);
  document.addEventListener("keydown", onKey, true);
}
