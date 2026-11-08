#!/usr/bin/env node
/**
 * 一条命令同步所有版本号。
 *
 * 版本单一来源：src-tauri/tauri.conf.json 的 version（与 publish.mjs 一致）。
 * 改动范围：
 *   1. src-tauri/tauri.conf.json    JSON 的 "version"
 *   2. package.json                 JSON 的 "version"
 *   3. Cargo.toml                   [workspace.package] 的 version
 *   4. src-tauri/Cargo.toml         [package] 的 version
 *   5. Cargo.lock                   三个 workspace 包（novel-cli / novel-core /
 *                                  novel-reader）的 version
 *
 * crates/* 用 version.workspace = true 继承，不需要改。
 * Cargo.lock 必须一起改：否则 Cargo.toml 与锁文件不一致，CI 里带 --locked
 * 的命令会直接失败（参考项目就踩过这个坑）。
 *
 * 用法：
 *   pnpm bump                  查看当前版本与用法
 *   pnpm bump 0.1.1            指定版本
 *   pnpm bump patch|minor|major  按语义化版本递增
 *
 * 只改文本；不 commit、不打 tag、不构建、不联网。写入失败会整体回滚。
 */
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const p = (...parts) => path.join(root, ...parts);
const read = (file) => fs.readFileSync(p(file), "utf8");

const SOURCE = "src-tauri/tauri.conf.json";
const DECLS = [
  { file: SOURCE, kind: "json" },
  { file: "package.json", kind: "json" },
  { file: "Cargo.toml", kind: "cargo" },
  { file: "src-tauri/Cargo.toml", kind: "cargo" },
];
const LOCK = "Cargo.lock";
const WORKSPACE_PKGS = new Set(["novel-cli", "novel-core", "novel-reader"]);

const JSON_RE = /^(\s*"version"\s*:\s*")[^"]+(")/m;
const CARGO_RE = /^(version\s*=\s*")[^"]+(")/m;
const SEMVER = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;

/** 读某个声明文件里的版本号（与 publish.mjs 的读法保持一致） */
function readVersion(file) {
  const src = read(file);
  const m = file.endsWith(".json")
    ? src.match(/^\s*"version"\s*:\s*"([^"]+)"/m)
    : src.match(/^version\s*=\s*"([^"]+)"/m);
  if (!m) throw new Error(`读不到版本号：${file}`);
  return m[1];
}

/** 只替换第一处版本声明，保留原文件的缩进 / 注释 / 换行风格 */
function replaceOnce(src, re, to, file) {
  let count = 0;
  const out = src.replace(re, (_whole, head, tail) => {
    count += 1;
    return head + to + tail;
  });
  if (count !== 1) {
    throw new Error(`${file}：期望恰好 1 处版本声明，实际匹配到 ${count} 处`);
  }
  return out;
}

/**
 * 扫描 Cargo.lock 的 [[package]] 块，只认这三个 workspace 包。
 * 传入 fn 时把该块的 version 换成 fn(old) 并返回新文本；不传则只读取。
 * 绝不全局替换 version —— 第三方 crate 也可能恰好是同一个版本号。
 */
function mapLock(src, fn) {
  const nl = src.includes("\r\n") ? "\r\n" : "\n";
  const lines = src.split(/\r?\n/);
  let inTarget = false;
  const versions = [];

  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    if (line.startsWith("[[")) {
      inTarget = false;
      continue;
    }
    const named = line.match(/^name = "([^"]+)"$/);
    if (named) {
      inTarget = WORKSPACE_PKGS.has(named[1]);
      continue;
    }
    if (inTarget && /^version = "/.test(line)) {
      const old = line.match(/^version = "([^"]+)"/)[1];
      versions.push(old);
      if (fn) lines[i] = `version = "${fn(old)}"`;
      inTarget = false;
    }
  }
  return { text: fn ? lines.join(nl) : src, versions };
}

function bumpSemver(current, part) {
  const m = current.match(/^(\d+)\.(\d+)\.(\d+)/);
  if (!m) throw new Error(`当前版本 ${current} 不是 x.y.z，无法自动递增，请显式指定版本号`);
  let major = Number(m[1]);
  let minor = Number(m[2]);
  let patch = Number(m[3]);
  if (part === "major") {
    major += 1;
    minor = 0;
    patch = 0;
  } else if (part === "minor") {
    minor += 1;
    patch = 0;
  } else {
    patch += 1;
  }
  return `${major}.${minor}.${patch}`;
}

function usage(current) {
  console.log(`当前版本：${current}（来源：${SOURCE}）`);
  console.log("");
  console.log("用法：pnpm bump <版本号 | patch | minor | major>");
  console.log(`  pnpm bump 0.1.1       指定版本`);
  console.log(`  pnpm bump patch        → ${bumpSemver(current, "patch")}`);
  console.log(`  pnpm bump minor        → ${bumpSemver(current, "minor")}`);
  console.log(`  pnpm bump major        → ${bumpSemver(current, "major")}`);
  console.log("");
  console.log("同步修改 4 处版本声明 + Cargo.lock 的 3 个 workspace 包；不 commit、不打 tag。");
}

function rollback(done) {
  for (const item of done.reverse()) {
    try {
      fs.writeFileSync(p(item.file), item.before);
    } catch (err) {
      console.error(`  ! 回滚 ${item.file} 失败：${err.message}`);
    }
  }
}

function main() {
  const arg = process.argv[2];
  const current = readVersion(SOURCE);

  if (!arg) {
    usage(current);
    return;
  }

  let to;
  if (["patch", "minor", "major"].includes(arg)) {
    to = bumpSemver(current, arg);
  } else if (SEMVER.test(arg)) {
    to = arg;
  } else {
    console.error(`版本号不合法：${arg}\n需要 x.y.z（可带 -pre / +build），或 patch / minor / major`);
    process.exit(1);
  }
  if (to === current) {
    console.error(`版本号没变化（仍是 ${current}）`);
    process.exit(1);
  }
  const rank = (v) => {
    const m = v.match(/^(\d+)\.(\d+)\.(\d+)/);
    return m ? Number(m[1]) * 1e6 + Number(m[2]) * 1e3 + Number(m[3]) : 0;
  };
  if (rank(to) < rank(current)) {
    console.log(`注意：${to} 低于当前版本 ${current}（降版本）`);
  }

  // 改之前先确认四处声明本来就一致，避免在半改状态上继续叠加
  const before = DECLS.map((d) => ({ file: d.file, version: readVersion(d.file) }));
  const drifted = before.filter((d) => d.version !== current);
  if (drifted.length) {
    console.error(`改之前版本号就不一致（以 ${SOURCE} 的 ${current} 为准）：`);
    for (const d of drifted) console.error(`  ${d.file}: ${d.version}`);
    console.error("请先手工修正，或运行 pnpm release 看完整校验输出。");
    process.exit(1);
  }
  const lockBefore = mapLock(read(LOCK), null);
  if (lockBefore.versions.length !== WORKSPACE_PKGS.size
      || lockBefore.versions.some((v) => v !== current)) {
    console.error(`Cargo.lock 中 workspace 包版本异常：${lockBefore.versions.join(", ")}（期望 ${WORKSPACE_PKGS.size} 个 ${current}）`);
    process.exit(1);
  }

  // 全部在内存里算好，再一次性落盘
  const writes = DECLS.map((d) => {
    const src = read(d.file);
    return {
      file: d.file,
      before: src,
      after: replaceOnce(src, d.kind === "json" ? JSON_RE : CARGO_RE, to, d.file),
    };
  });
  writes.push({
    file: LOCK,
    before: read(LOCK),
    after: mapLock(read(LOCK), () => to).text,
  });

  const done = [];
  try {
    for (const item of writes) {
      fs.writeFileSync(p(item.file), item.after);
      done.push(item);
    }
  } catch (err) {
    rollback(done);
    console.error(`写入失败，已回滚：${err.message}`);
    process.exit(1);
  }

  // 自检：四处声明 + Cargo.lock 必须全部是新版本，否则整体回滚
  const bad = DECLS.filter((d) => readVersion(d.file) !== to).map((d) => d.file);
  const lockAfter = mapLock(read(LOCK), null).versions;
  if (bad.length || lockAfter.some((v) => v !== to)) {
    rollback(done);
    console.error(`自检失败，已回滚：声明异常 ${bad.join(", ") || "无"}；lock ${lockAfter.join(", ")}`);
    process.exit(1);
  }

  console.log(`${current} → ${to}`);
  for (const item of writes) console.log(`  ✓ ${item.file}`);
  console.log(`自检通过：4 处声明 + Cargo.lock 的 ${lockAfter.length} 个 workspace 包均为 ${to}`);
  if (rank(to) < rank(current)) return; // 降版本多半是回退，别提示发版
  console.log("");
  console.log("下一步（脚本不代劳）：");
  console.log(`  pnpm check:version v${to}    # 与 CI 同一套校验`);
  console.log(`  git add ${DECLS.map((d) => d.file).join(" ")} ${LOCK}`);
  console.log(`  git commit -m "chore: 版本号更新到 v${to}"`);
  console.log("  git push origin main");
  console.log(`  git tag -a v${to} -m "v${to}" && git push origin v${to}`);
}

try {
  main();
} catch (err) {
  console.error(`出错：${err.message}`);
  process.exit(1);
}
