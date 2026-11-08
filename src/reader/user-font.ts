/**
 * 本地字体注册。
 *
 * 走 `FontFace` + ArrayBuffer：Tauri 侧把字体读成原始字节（二进制通道，
 * 不经过 JSON），这里包成 FontFace 塞进 document.fonts，再用 font-family 引用。
 *
 * 为什么不用 `convertFileSrc` 的 asset:// 协议：那需要给 assetProtocol 配作用域，
 * 等于把任意本地路径暴露给 WebView，为一个字体不值当。
 */

import { api } from "../core/ipc";

/** 字体族名。带前缀避免和系统字体撞名。 */
export const USER_FONT_FAMILY = "NovelReaderUserFont";

let loadedPath: string | null = null;
let loading: Promise<void> | null = null;

/** 注册（或复用）用户字体。重复调用同一路径不会重复读盘。 */
export async function registerUserFont(path: string): Promise<void> {
  if (loadedPath === path) return;
  if (loading) await loading;

  loading = (async () => {
    const buf = await api.readFontBytes(path);
    if (!(buf instanceof ArrayBuffer) || buf.byteLength === 0) {
      throw new Error("字体文件是空的");
    }

    const face = new FontFace(USER_FONT_FAMILY, buf);
    await face.load();
    document.fonts.add(face);
    loadedPath = path;
  })();

  try {
    await loading;
  } finally {
    loading = null;
  }
}

/** 摘掉已注册的字体，并回落到内置字体栈。 */
export function unregisterUserFont(): void {
  for (const face of [...document.fonts]) {
    if (face.family === USER_FONT_FAMILY) document.fonts.delete(face);
  }
  loadedPath = null;
}

export function isUserFontReady(path: string): boolean {
  return loadedPath === path;
}
