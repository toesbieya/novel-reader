/**
 * 轻量对话框：输入 / 二次确认 / 选目录。
 *
 * 不用 window.prompt/confirm 的原因：
 *  - WebView2 里原生弹窗样式和暖纸色主题完全不搭；
 *  - 删除目录需要说明「磁盘原文件不受影响」，原生 confirm 讲不清；
 *  - 选目录需要一棵带缩进的列表，原生没有。
 *
 * 所有对话框共用一层外壳，Enter 提交、Esc 取消、点遮罩取消。
 */

import { h } from "./dom";

export interface PromptOptions {
  title: string;
  description?: string;
  value?: string;
  placeholder?: string;
  okText?: string;
  /** 返回错误文案表示不通过；返回 null 放行 */
  validate?: (value: string) => string | null;
}

export interface ConfirmOptions {
  title: string;
  message: string;
  okText?: string;
  danger?: boolean;
}

export interface ChooseItem<T> {
  label: string;
  hint?: string;
  depth?: number;
  value: T;
  disabled?: boolean;
}

interface Shell {
  body: HTMLElement;
  foot: HTMLElement;
  close: () => void;
}

function openModal(title: string, subtitle: string | null): Shell {
  const body = h("div", { class: "modal-body" });
  const foot = h("div", { class: "modal-foot" });

  const close = (): void => {
    document.removeEventListener("keydown", onKey, true);
    mask.remove();
  };

  const onKey = (e: KeyboardEvent): void => {
    if (e.key === "Escape") {
      e.preventDefault();
      close();
    }
  };

  const card = h(
    "div",
    { class: "modal-card", role: "dialog", "aria-modal": "true" },
    h(
      "div",
      { class: "modal-head" },
      h("h2", { class: "modal-title" }, title),
      h(
        "button",
        { class: "modal-x", type: "button", "aria-label": "关闭", onClick: () => close() },
        "×",
      ),
    ),
    subtitle ? h("p", { class: "modal-sub" }, subtitle) : null,
    body,
    foot,
  );

  const mask = h(
    "div",
    {
      class: "modal-mask",
      onPointerdown: (e: Event) => {
        if (e.target === mask) close();
      },
    },
    card,
  );

  document.body.appendChild(mask);
  document.addEventListener("keydown", onKey, true);

  return { body, foot, close };
}

function button(label: string, cls: string, onClick: () => void): HTMLElement {
  return h("button", { class: cls, type: "button", onClick }, label);
}

export function promptText(opts: PromptOptions): Promise<string | null> {
  return new Promise((resolve) => {
    let done = false;
    const settle = (v: string | null): void => {
      if (done) return;
      done = true;
      shell.close();
      resolve(v);
    };

    const shell = openModal(opts.title, opts.description ?? null);
    const err = h("p", { class: "modal-err" });

    const input = h("input", {
      class: "modal-input",
      type: "text",
      value: opts.value ?? "",
      placeholder: opts.placeholder ?? "",
      onKeyDown: (e: Event) => {
        const ev = e as KeyboardEvent;
        if (ev.key === "Enter") {
          ev.preventDefault();
          submit();
        }
      },
    });

    const submit = (): void => {
      const raw = input.value.trim();
      const msg = opts.validate ? opts.validate(raw) : raw ? null : "不能为空";
      if (msg) {
        err.textContent = msg;
        input.focus();
        return;
      }
      settle(raw);
    };

    shell.body.appendChild(input);
    shell.body.appendChild(err);
    shell.foot.appendChild(button("取消", "btn-ghost", () => settle(null)));
    shell.foot.appendChild(button(opts.okText ?? "确定", "btn-primary", submit));

    input.focus();
    input.select();
  });
}

export function confirmAction(opts: ConfirmOptions): Promise<boolean> {
  return new Promise((resolve) => {
    let done = false;
    const settle = (v: boolean): void => {
      if (done) return;
      done = true;
      shell.close();
      resolve(v);
    };

    const shell = openModal(opts.title, null);
    shell.body.appendChild(h("p", { class: "modal-text" }, opts.message));
    shell.foot.appendChild(button("取消", "btn-ghost", () => settle(false)));
    shell.foot.appendChild(
      button(opts.okText ?? "确定", opts.danger ? "btn-danger" : "btn-primary", () => settle(true)),
    );

    shell.foot.querySelector("button")?.focus();
  });
}

export type DataDirAction = "open" | "change";

/**
 * 数据目录面板：展示当前位置，返回用户选的动作（null = 关闭）。
 * 只负责界面，不碰 IPC——迁移流程留在调用方，基础件不依赖 api。
 */
export function showDataDirDialog(opts: { current: string }): Promise<DataDirAction | null> {
  return new Promise((resolve) => {
    let done = false;
    const settle = (value: DataDirAction | null): void => {
      if (done) return;
      done = true;
      shell.close();
      resolve(value);
    };

    const shell = openModal(
      "设置",
      "书库、阅读进度、阅读偏好和窗口状态都存在一个 library.db 里。",
    );

    const path = h("div", { class: "data-dir-path", title: opts.current }, opts.current);
    shell.body.appendChild(
      h(
        "div",
        { class: "data-dir-current" },
        h("span", { class: "data-dir-label" }, "数据目录 · 当前位置"),
        path,
      ),
    );
    shell.body.appendChild(
      h(
        "p",
        { class: "modal-text" },
        "更改后会把当前书库复制到新目录并重启应用。原目录不会被删除；" +
          "如果目标目录里已经有 library.db，会先问你是直接切换还是取消。",
      ),
    );

    shell.foot.appendChild(button("关闭", "btn-ghost", () => settle(null)));
    shell.foot.appendChild(button("打开目录", "btn-ghost", () => settle("open")));
    shell.foot.appendChild(button("更改…", "btn-primary", () => settle("change")));
  });
}

/**
 * 选一个目录。返回 undefined 表示用户取消；
 * 选中「根目录」时返回 null——所以取消不能用 null 表示，两者必须分开。
 */
export function chooseFolder<T>(opts: {
  title: string;
  description?: string;
  items: ChooseItem<T>[];
  current?: T;
}): Promise<T | undefined> {
  return new Promise((resolve) => {
    let done = false;
    // 没给当前值就默认选第一项，避免「点了确定却没选」的空转
    let picked: T | undefined = opts.current ?? opts.items.find((i) => !i.disabled)?.value;

    const settle = (v: T | undefined): void => {
      if (done) return;
      done = true;
      shell.close();
      resolve(v);
    };

    const shell = openModal(opts.title, opts.description ?? null);
    const list = h("div", { class: "modal-list" });

    const rows = opts.items.map((item) => {
      const row = h(
        "button",
        {
          class: `modal-row${item.disabled ? " is-disabled" : ""}`,
          type: "button",
          style: { paddingLeft: `${12 + (item.depth ?? 0) * 16}px` },
          disabled: item.disabled ? true : null,
          onClick: () => {
            if (item.disabled) return;
            picked = item.value;
            for (const r of rows) r.classList.remove("is-picked");
            row.classList.add("is-picked");
          },
          onDblClick: () => {
            if (item.disabled) return;
            settle(item.value);
          },
        },
        h("span", { class: "modal-row-label" }, item.label),
        item.hint ? h("span", { class: "modal-row-hint" }, item.hint) : null,
      );
      if (Object.is(item.value, picked)) {
        row.classList.add("is-picked");
      }
      return row;
    });

    for (const row of rows) list.appendChild(row);
    shell.body.appendChild(list);

    shell.foot.appendChild(button("取消", "btn-ghost", () => settle(undefined)));
    shell.foot.appendChild(
      button("移动到这里", "btn-primary", () => {
        if (picked === undefined) return;
        settle(picked);
      }),
    );
  });
}
