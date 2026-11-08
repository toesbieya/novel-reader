/**
 * 单例右键菜单。
 *
 * 只保留一个实例：反复右键时旧菜单先销毁，避免浮层叠一堆。
 * 关闭时机包括外部按下、Esc、滚动、失焦、窗口尺寸变化——
 * 少一种就会出现「菜单浮在页面上关不掉」的尴尬。
 */

import { h } from "./dom";

export interface MenuItem {
  label?: string;
  /** 右侧灰色注记，用来解释为什么某项不可点 */
  hint?: string;
  danger?: boolean;
  disabled?: boolean;
  /** 单选菜单里表示当前选中项，勾号跟在文案后面 */
  checked?: boolean;
  /** 分隔线 */
  sep?: boolean;
  onClick?: () => void;
}

let cleanup: (() => void) | null = null;

export function closeContextMenu(): void {
  const fn = cleanup;
  cleanup = null;
  fn?.();
}

export function showContextMenu(x: number, y: number, items: MenuItem[]): void {
  closeContextMenu();

  const el = h("div", { class: "ctx-menu", role: "menu" });
  el.style.visibility = "hidden";

  for (const item of items) {
    if (item.sep) {
      el.appendChild(h("div", { class: "ctx-sep" }));
      continue;
    }

    const label = h(
      "span",
      { class: "ctx-label" },
      item.label ?? "",
      item.checked ? h("span", { class: "ctx-check" }, "✓") : null,
    );
    const hint = item.hint ? h("span", { class: "ctx-hint" }, item.hint) : null;

    if (item.disabled) {
      el.appendChild(
        h("div", { class: "ctx-item is-disabled", role: "menuitem" }, label, hint),
      );
      continue;
    }

    el.appendChild(
      h(
        "button",
        {
          class: `ctx-item${item.danger ? " is-danger" : ""}`,
          role: "menuitem",
          type: "button",
          onClick: () => {
            // 先关再执行：动作里可能弹对话框或刷新列表
            closeContextMenu();
            item.onClick?.();
          },
        },
        label,
        hint,
      ),
    );
  }

  document.body.appendChild(el);

  // 先插入再量尺寸，才能把菜单收进视口（右下角右键时最容易溢出）
  const rect = el.getBoundingClientRect();
  const left = Math.max(8, Math.min(x, window.innerWidth - rect.width - 8));
  const top = Math.max(8, Math.min(y, window.innerHeight - rect.height - 8));
  el.style.left = `${left}px`;
  el.style.top = `${top}px`;
  el.style.visibility = "";

  const onDown = (e: PointerEvent): void => {
    if (e.target instanceof Node && el.contains(e.target)) return;
    closeContextMenu();
  };
  const onKey = (e: KeyboardEvent): void => {
    if (e.key === "Escape") closeContextMenu();
  };

  document.addEventListener("pointerdown", onDown, true);
  document.addEventListener("keydown", onKey, true);
  window.addEventListener("blur", closeContextMenu);
  window.addEventListener("resize", closeContextMenu);
  window.addEventListener("wheel", closeContextMenu, { passive: true });

  cleanup = () => {
    document.removeEventListener("pointerdown", onDown, true);
    document.removeEventListener("keydown", onKey, true);
    window.removeEventListener("blur", closeContextMenu);
    window.removeEventListener("resize", closeContextMenu);
    window.removeEventListener("wheel", closeContextMenu);
    el.remove();
  };
}
