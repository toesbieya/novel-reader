import { h } from "./core/dom";
import { onBookReindexed, type BookSummary } from "./core/ipc";
import { ReaderView } from "./views/reader";
import { ShelfView } from "./views/shelf";

const mount = document.getElementById("app");
if (!mount) throw new Error("找不到 #app 挂载点");

const root: HTMLElement = mount;

/**
 * 关掉 WebView2 的原生右键菜单。
 *
 * 原生菜单里是「刷新 / 另存为 / 打印 / 检查」这类浏览器动作，对阅读器毫无意义；
 * 书架和阅读页该有的菜单都由 `core/menu.ts` 自绘。
 * 只给**文本输入**留原生菜单——右键粘贴、全选还得靠它。
 */
const TEXT_ENTRY =
  "input[type='text'], input[type='search'], input:not([type]), textarea, [contenteditable='true']";
document.addEventListener("contextmenu", (e) => {
  const target = e.target instanceof Element ? e.target.closest(TEXT_ENTRY) : null;
  if (target) return;
  e.preventDefault();
});

// 全局 Toast：书文件在应用外被改动、目录已重建时必须让用户看到。
// 挂在 app 层而不是书架里——重建发生在读章节 / 搜索时，那时书架已经卸载了。
const toast = h("div", { class: "toast" }, "");
document.body.appendChild(toast);
let toastTimer = 0;
/** bookId → 上次提示的时间戳；5 分钟内同一本书只提示一次，避免刷屏 */
const lastNoticed = new Map<number, number>();

function showToast(text: string): void {
  toast.textContent = text;
  toast.classList.add("is-on");
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => toast.classList.remove("is-on"), 3600);
}

void onBookReindexed((e) => {
  const now = Date.now();
  if (now - (lastNoticed.get(e.bookId) ?? 0) < 5 * 60_000) return;
  lastNoticed.set(e.bookId, now);
  showToast(`《${e.title}》的文件变了，目录已重建`);
});

export function showShelf(): void {
  root.replaceChildren(ShelfView(openBook));
}

function openBook(book: BookSummary): void {
  root.replaceChildren(ReaderView(book.id, book.title, showShelf));
}
