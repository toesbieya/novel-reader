/**
 * 极简响应式基座。
 *
 * 设计取舍：不做 VDOM。effect 直接读写真实 DOM，signal 变动同步通知。
 * 同步而非微任务批处理，是因为阅读页需要在写入正文后立刻量高度做分页，
 * 异步 flush 会让测量拿到上一帧的布局。
 *
 * 防重入靠 activeObserver === run 这一句：effect 内改自己依赖的 signal 不会无限递归，
 * 而是等下一轮外部变更再跑（牺牲了自触发，换来了不会爆栈）。
 */

export interface Signal<T> {
  (): T;
  peek(): T;
  set(value: T): void;
  update(fn: (prev: T) => T): void;
}

let activeObserver: (() => void) | null = null;

export function signal<T>(initial: T): Signal<T> {
  let value = initial;
  const subscribers = new Set<() => void>();

  const read = (() => {
    if (activeObserver) subscribers.add(activeObserver);
    return value;
  }) as Signal<T>;

  read.peek = () => value;

  read.set = (next: T) => {
    if (Object.is(next, value)) return;
    value = next;
    // 拷贝一份，防止 effect 内部取消订阅时改动集合导致跳过
    for (const run of [...subscribers]) run();
  };

  read.update = (fn) => read.set(fn(value));

  return read;
}

/** 立即执行一次并在依赖变动时重跑，返回取消订阅函数。 */
export function effect(fn: () => void): () => void {
  let disposed = false;

  const run = () => {
    if (disposed) return;
    if (activeObserver === run) return;
    const prev = activeObserver;
    activeObserver = run;
    try {
      fn();
    } finally {
      activeObserver = prev;
    }
  };

  run();

  return () => {
    disposed = true;
  };
}
