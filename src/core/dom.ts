/**
 * 手写 DOM 构造器，替代 JSX。
 * 约定：on* 属性绑事件，class/style/dataset/html/text 特殊处理，其余优先走属性赋值。
 */

type Child = Node | string | number | null | undefined | false;

export interface Attrs {
  class?: string;
  style?: Record<string, string>;
  dataset?: Record<string, string>;
  html?: string;
  text?: string;
  [key: string]: unknown;
}

export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs?: Attrs | null,
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);

  if (attrs) {
    for (const [key, val] of Object.entries(attrs)) {
      if (val === null || val === undefined || val === false) continue;

      switch (key) {
        case "class":
          el.className = String(val);
          break;
        case "style":
          if (typeof val === "object") Object.assign(el.style, val);
          else el.setAttribute("style", String(val));
          break;
        case "dataset":
          if (typeof val === "object") {
            for (const [k, v] of Object.entries(val as Record<string, string>)) {
              el.dataset[k] = v;
            }
          }
          break;
        case "html":
          el.innerHTML = String(val);
          break;
        case "text":
          el.textContent = String(val);
          break;
        case "for":
          el.setAttribute("for", String(val));
          break;
        default:
          if (key.startsWith("on") && typeof val === "function") {
            el.addEventListener(key.slice(2).toLowerCase(), val as EventListener);
          } else if (key in el) {
            (el as unknown as Record<string, unknown>)[key] = val;
          } else {
            el.setAttribute(key, String(val));
          }
      }
    }
  }

  appendAll(el, children);
  return el;
}

/** 文档片段，用于批量插入。 */
export function frag(...children: Child[]): DocumentFragment {
  const f = document.createDocumentFragment();
  appendAll(f, children);
  return f;
}

export function appendAll(parent: Node, children: Child[]): void {
  for (const child of children) {
    if (child === null || child === undefined || child === false) continue;
    parent.appendChild(
      typeof child === "string" || typeof child === "number"
        ? document.createTextNode(String(child))
        : child,
    );
  }
}

export function clear(el: Element): void {
  while (el.firstChild) el.removeChild(el.firstChild);
}
