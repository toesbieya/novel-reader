#!/usr/bin/env node
/**
 * 版本一致性校验 —— 本地与 CI 共用同一套规则。
 *
 * 以 src-tauri/tauri.conf.json 的 version 为单一来源，检查：
 *   1. package.json / Cargo.toml / src-tauri/Cargo.toml 三处声明
 *   2. Cargo.lock 里 novel-cli / novel-core / novel-reader 三个 workspace 包
 *   3. 可选：传入的 tag 必须等于 v${version}
 *
 * 用法：
 *   pnpm check:version                    # 只校验文件（本地日常）
 *   pnpm check:version v0.1.0            # 连 tag 一起校验
 *   node scripts/check-version.mjs v0.1.0
 *   RELEASE_VERSION=v0.1.0 node scripts/check-version.mjs
 *
 * 只依赖 Node 内置模块，因此 CI 里可以放在 pnpm install 之前跑，尽早失败。
 * 这是参考 hadc188/reader 的 scripts/check-release-metadata.mjs 思路，
 * 额外补上了 Cargo.lock 校验（他们曾因锁文件不同步踩过 --locked 的坑）。
 */
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const p = (...parts) => path.join(root, ...parts);
const read = (file) => fs.readFileSync(p(file), "utf8");

const SOURCE = "src-tauri/tauri.conf.json";
const DECLS = [
  SOURCE,
  "package.json",
  "Cargo.toml",
  "src-tauri/Cargo.toml",
];
const LOCK = "Cargo.lock";
const WORKSPACE_PKGS = new Set(["novel-cli", "novel-core", "novel-reader"]);

/** 读某个声明文件里的版本号（与 publish.mjs / bump-version.mjs 的读法一致） */
function readVersion(file) {
  const src = read(file);
  const m = file.endsWith(".json")
    ? src.match(/^\s*"version"\s*:\s*"([^"]+)"/m)
    : src.match(/^version\s*=\s*"([^"]+)"/m);
  if (!m) throw new Error(`读不到版本号：${file}`);
  return m[1];
}

/** 只扫 [[package]] 块，绝不全局匹配 version —— 第三方 crate 也可能同版本 */
function lockVersions(src) {
  const lines = src.split(/\r?\n/);
  let name = null;
  const out = [];
  for (const line of lines) {
    if (line.startsWith("[[")) {
      name = null;
      continue;
    }
    const named = line.match(/^name = "([^"]+)"$/);
    if (named) {
      name = named[1];
      continue;
    }
    if (name && WORKSPACE_PKGS.has(name) && /^version = "/.test(line)) {
      out.push({ name, version: line.match(/^version = "([^"]+)"/)[1] });
      name = null;
    }
  }
  return out;
}

function main() {
  // 不读 GITHUB_REF_NAME：workflow_dispatch 时它是分支名，会误判成 tag
  const tag = process.argv[2] || process.env.RELEASE_VERSION || "";
  const base = readVersion(SOURCE);
  const problems = [];

  for (const file of DECLS) {
    const v = readVersion(file);
    if (v !== base) problems.push(`${file}: ${v}（应为 ${base}）`);
  }

  const locks = lockVersions(read(LOCK));
  if (locks.length !== WORKSPACE_PKGS.size) {
    problems.push(`Cargo.lock: 找到 ${locks.length} 个 workspace 包，期望 ${WORKSPACE_PKGS.size} 个`);
  }
  for (const item of locks) {
    if (item.version !== base) {
      problems.push(`Cargo.lock [${item.name}]: ${item.version}（应为 ${base}）`);
    }
  }

  if (tag) {
    if (!tag.startsWith("v")) problems.push(`tag 必须以 v 开头：${tag}`);
    else if (tag !== `v${base}`) problems.push(`tag ${tag} 与项目版本 ${base} 不一致（应为 v${base}）`);
  }

  if (problems.length) {
    console.error(`版本校验失败（以 ${SOURCE} 的 ${base} 为准）：`);
    for (const item of problems) console.error(`  ✗ ${item}`);
    console.error("修正方式：pnpm bump <版本号|patch|minor|major>");
    process.exit(1);
  }

  const scope = `4 处声明 + Cargo.lock ${locks.length} 个包`;
  console.log(`版本一致：v${base}（${scope}${tag ? ` + tag ${tag}` : ""}）`);
}

try {
  main();
} catch (err) {
  console.error(`版本校验出错：${err.message}`);
  process.exit(1);
}
