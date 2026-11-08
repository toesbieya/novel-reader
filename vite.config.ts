import { defineConfig } from "vite";

// Tauri 会把 devUrl 指向 1420，端口必须固定，不能被 vite 自动顺延。
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  // 相对路径：dist 既能被 Tauri 以 tauri://localhost 加载，也便于换目录部署。
  // 浏览器预览必须走 http（pnpm dev / vite preview）；file:// 下 ES module 会被 CORS 拦住。
  base: "./",
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: {
      // Rust 侧变动不触发前端热更新，避免刷屏
      ignored: ["**/src-tauri/**", "**/crates/**"],
    },
  },
  build: {
    // 这是交给 esbuild 的**最低语法目标**，保守取 chrome120；
    // 运行时 WebView2 是 156，但产物不该依赖最高版本的语法。
    target: "chrome120",
    minify: "esbuild",
    sourcemap: false,
    assetsInlineLimit: 4096,
  },
});
