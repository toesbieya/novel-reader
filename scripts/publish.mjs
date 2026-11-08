#!/usr/bin/env node
/**
 * 发布脚本：把 `pnpm tauri build` 的产物同步到 release/。
 *
 * 用法：pnpm release（出包后跑；脚本不会自己编译）
 * 做的事：
 *  1. 以 src-tauri/tauri.conf.json 的 version 为单一源，校验另外三处版本号一致；
 *  2. 检查绿色版 exe / NSIS 安装包确实存在；
 *  3. 检测 NovelReader.exe 是否正在运行（**不结束用户进程**），尝试「旧文件改名 → 写新文件」的安全替换；
 *  4. 生成 release/SHA256SUMS.txt。
 */
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const p = (...parts) => path.join(root, ...parts);
const readJson = (file) => JSON.parse(fs.readFileSync(p(file), "utf8"));

function cargoVersion(file) {
  const m = fs.readFileSync(p(file), "utf8").match(/^\s*version\s*=\s*"([^"]+)"/m);
  return m ? m[1] : null;
}

// 1. 版本单一源
const version = readJson("src-tauri/tauri.conf.json").version;
const declared = {
  "package.json": readJson("package.json").version,
  "Cargo.toml": cargoVersion("Cargo.toml"),
  "src-tauri/Cargo.toml": cargoVersion("src-tauri/Cargo.toml"),
};
const mismatched = Object.entries(declared).filter(([, v]) => v !== version);
if (mismatched.length) {
  console.error(`版本号不一致（以 src-tauri/tauri.conf.json 的 ${version} 为准）：`);
  for (const [file, v] of mismatched) console.error(`  ${file}: ${v ?? "读不到"}`);
  process.exit(1);
}
console.log(`版本一致：${version}`);

// 2. 产物
const greenSrc = p("target/release/novel-reader.exe");
const setupSrc = p(`target/release/bundle/nsis/Novel Reader_${version}_x64-setup.exe`);
for (const f of [greenSrc, setupSrc]) {
  if (!fs.existsSync(f)) {
    console.error(`缺少构建产物：${f}\n请先运行 pnpm tauri build`);
    process.exit(1);
  }
}

// 3. 占用检测：只读，不 kill
function novelReaderRunning() {
  if (process.platform !== "win32") return false;
  try {
    const out = execFileSync(
      "tasklist",
      ["/FI", "IMAGENAME eq NovelReader.exe", "/FO", "CSV", "/NH"],
      { encoding: "utf8" },
    );
    return /NovelReader\.exe/i.test(out);
  } catch {
    return false;
  }
}
const busy = novelReaderRunning();
if (busy) console.log("检测到 NovelReader.exe 正在运行：只做安全替换，不会结束进程。");

// 4. 同步（Windows 允许重命名正在运行的映像，但不允许覆盖写）
const release = p("release");
fs.mkdirSync(release, { recursive: true });
const greenDest = p("release/NovelReader.exe");
const setupDest = p(`release/Novel Reader_${version}_x64-setup.exe`);

function copySafe(src, dest) {
  if (!fs.existsSync(dest)) {
    fs.copyFileSync(src, dest);
    return "新增";
  }
  const old = `${dest}.old`;
  try {
    fs.renameSync(dest, old);
  } catch (err) {
    throw new Error(`${path.basename(dest)} 被占用且无法改名：${err.message}\n请关闭阅读器后重试`);
  }
  try {
    fs.copyFileSync(src, dest);
  } catch (err) {
    fs.renameSync(old, dest); // 还原，别把用户的 exe 弄丢
    throw new Error(`写入 ${path.basename(dest)} 失败，已还原：${err.message}`);
  }
  try {
    fs.rmSync(old, { force: true });
  } catch {
    console.log(`  旧版备份保留在 ${path.basename(old)}（进程占用，可稍后手动删）`);
  }
  return "替换";
}

try {
  console.log(`绿色版：${copySafe(greenSrc, greenDest)}`);
  console.log(`安装包：${copySafe(setupSrc, setupDest)}`);
} catch (err) {
  console.error(err.message);
  process.exit(1);
}

// 5. 校验和
const sha = (f) => crypto.createHash("sha256").update(fs.readFileSync(f)).digest("hex");
fs.writeFileSync(
  p("release/SHA256SUMS.txt"),
  [greenDest, setupDest].map((f) => `${sha(f)}  ${path.basename(f)}`).join("\n") + "\n",
);
console.log("已生成 release/SHA256SUMS.txt");

if (busy) console.log("阅读器仍在运行，重启后才会加载新版本。");
