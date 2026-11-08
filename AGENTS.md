# novel-reader · 工程文档

> 面向接手 / 维护这份代码的工程师与 AI Agent。用户向说明见 [README.md](README.md)。
> 最后更新：2026-11-08

---

## 0. 现状与边界

Windows 本地 TXT / EPUB 阅读器，v0.1，功能闭环、可日常使用。导入 → 书架 → 目录 → 分页阅读 →
搜索高亮 → 进度记忆全部打通，另含 5 级目录管理、拖拽归类与排序、导入文件夹、
本地 / 系统字体、四套主题、点击翻页、全文搜索按章分组。

**产品边界**：Win10/11 本地阅读器；完全离线、零副本、目录最多 5 级；删除只删记录；
导入、打开生成目录、恢复进度、全文搜索是硬性能指标（见 §5）。

**技术选型**：Tauri 2 + WebView2（包小、内存低、Win11 自带）；前端原生 TS + Vite 无框架
（正文不经过 VDOM）；引擎纯 Rust `novel-core`（mmap + 字节偏移，零 UI 依赖，可独立压测）；
SQLite（rusqlite bundled）单文件持久化。已否：Electron（重）、WPF（还原设计成本高）、
GPUI/egui（生态 / 排版风险）、React/Vue（收益小于运行时成本）。

---

## 1. 上手

### 1.1 环境

Rust stable（MSVC toolchain）、VS 2022 Build Tools + Windows SDK、Node 20+、pnpm。
Win11 自带 WebView2；Win10 需要 Evergreen Runtime。`cargo` 如果不在 `PATH`，
先把它加进去（rustup 默认装到 `~/.cargo/bin`，Windows 上常常没进环境变量）。

### 1.2 命令

```bash
pnpm dev                              # 前端开发服务器（localhost:1420，浏览器可跑，走 mock）
cargo build -p novel-reader           # 调试版 exe
cargo build --release -p novel-reader # 发布版 exe
pnpm tauri build                      # 只出绿色版 exe（target/release/novel-reader.exe）
pnpm release                          # 同步产物到 release/（版本校验 + 占用检测 + SHA256）
pnpm bump <版本|patch|minor|major>    # 同步四处版本声明 + Cargo.lock（不 commit / 不打 tag）
pnpm check:version [vX.Y.Z]           # 校验 4 处声明 + Cargo.lock + tag（本地与 CI 共用）

# 引擎侧：压测 / 造数据 / 验收 / 直接操作书架
cargo build --release -p novel-cli
./target/release/novel-cli.exe selftest          # 数据层 39 项验收，改 store 后必跑
cargo test -p novel-core                         # 引擎测试 56 项，改 novel-core 必跑
./target/release/novel-cli.exe bench <文件>       # 性能穿刺，输出对照红线
./target/release/novel-cli.exe lib-books <db>    # 直接看库里有什么
```

数据库：`%APPDATA%\com.novelreader.app\library.db`（debug 与 release 共用）。
书库、进度、阅读偏好、书架排序、窗口状态都在 `setting` 表；书架顶栏「设置 → 数据目录」可整体迁移。

### 1.3 七个必踩的坑

1. **pnpm 建符号链接可能被沙箱拦，而且不报错** —— 表现为 `pnpm add` 后 `node_modules` 里没有那个包、
   `tsc` 报"找不到模块"。解法：在**非沙箱**环境 `rm -rf node_modules && pnpm install`。
   `.npmrc` 已设 `node-linker=hoisted`，但仍需非沙箱。
2. **Tauri 把前端资源编进 exe，改 `dist/` 不会触发重编译** —— `pnpm build` 后直接 `cargo build`
   是空的（0.8 秒 Finished），跑起来还是旧界面。解法：`pnpm build && touch src-tauri/src/lib.rs && cargo build`，
   或者直接用 `pnpm tauri build`。
3. **`regex` crate 不支持反向引用** —— 写 `</\1>` 会**运行时 panic**（不是编译错误），
   而且只有真跑到那段代码才炸。
4. **`dragDropEnabled: true` 会把 Windows 上的 HTML5 拖拽一起废掉** —— 系统级拖放由 Tauri 接管，
   `draggable` / `dragstart` / `drop` 一个都不触发，浏览器 preview 里又是好的，很容易误判。
   关掉它会丢掉"拖文件进窗口导入"，所以书架拖拽改用 pointer 事件手写（`core/drag.ts`）。
   **不要**为了省事去关 `dragDropEnabled`。
5. **`overflow: hidden` 挡不住程序化滚动，`.focus()` 会把正文"滚走"** —— 抽屉关闭态的
   `translateX(100%)` 会在 `.reader` 右侧撑出 324px 可滚动溢出区。打开搜索抽屉时，80ms 后
   `searchInput.focus()` 正赶上 240ms 滑入动画，浏览器为了把输入框滚进视口会把 `scrollLeft` 推到 324，
   看起来就是"左右闪一下"。解法：`focus({ preventScroll: true })`。阅读页里任何
   `focus()` / `scrollIntoView()` 都要留意这条滚动链。
6. **点击翻页要在 `pointerup` 里立即执行，不能加延迟** —— 220ms 的等待会被感知成"点了没反应"。
   代价是左右热区里的双击选词会被第一下点击翻页吃掉（中间 40% 正常，拖选在所有区域正常）；
   双击的第二下靠 `e.detail > 1` 跳过，不会一次翻两页。长按 >700ms 或有选区也不翻。
   改这段要同时验证「单击右半边立即翻页」和「双击不翻两页」。
7. **`import_paths` / `import_folder` 不能改回同步命令** —— Tauri 的同步命令跑在主线程，
   66 本导入会把窗口消息循环一起堵死。必须 async + `spawn_blocking`，每本只在写库时短暂拿锁。

---

## 2. 目录地图

```
crates/novel-core/           纯 Rust 引擎，零 UI 依赖，可独立压测
├─ encoding.rs   编码探测（BOM → UTF-16 嗅探 → UTF-8 校验 → GB18030/Big5 打分）+ 按区间解码
├─ txt.rs        TXT 切章：Aho-Corasick 字节预筛 + 正则校验，产出字节偏移表
├─ search.rs     全文搜索：查询串按文件编码编码后做字节匹配 + 围绕本次命中截摘要
├─ epub.rs       container.xml → opf → spine + nav/ncx → HTML 转文本
├─ text_cache.rs EPUB 搜索纯文本缓存（零副本的唯一例外，见 §3 第 1 条）
├─ scan.rs       文件夹导入：递归扫描 txt / epub，产出「相对目录分量」，不碰数据库
├─ fonts.rs      系统字体族名枚举（解析 name/cmap/OS2，只保留中文字体）
├─ store.rs      SQLite 书架 / 5 级目录树 / 进度 / 文件失效 + 打开文档 LRU 缓存
├─ model.rs      ChapterMeta / Hit / SearchOutcome
└─ lib.rs        Document 枚举（Txt / Epub）统一入口

crates/novel-cli/src/main.rs  压测与运维工具，含合成语料生成器

src-tauri/src/lib.rs          27 个 Tauri 命令（薄层：拿锁 → 调引擎 → 错误转字符串）
src-tauri/tauri.conf.json     窗口 1280×820、dragDropEnabled、bundle.active=false（不打包）

src/                          前端，纯 TypeScript，无框架
├─ app.ts           视图切换（shelf ↔ reader）
├─ core/ipc.ts      **IPC 契约唯一来源**：类型 + api 方法 + 浏览器 mock
├─ core/reactive.ts signal/effect
├─ core/dom.ts      h() 构造器
├─ core/menu.ts     单例右键菜单（视口收敛 / Esc / 外部点击 / 滚动关闭）
├─ core/dialog.ts   自绘弹窗：promptText / confirmAction / chooseFolder
├─ core/drag.ts     指针拖拽（**不能用 HTML5 DnD**，见 §1.3 坑 4）
├─ core/shelf-prefs.ts 书架排序选项
├─ core/settings.ts     应用设置：经 IPC 读写 SQLite，迁移旧 localStorage
├─ reader/paginator.ts  **分页引擎**（CSS 多栏，双向字符偏移换算）
├─ reader/user-font.ts  本地字体：FontFace + ArrayBuffer 注册/卸载
├─ reader/prefs.ts      阅读偏好类型与默认值
├─ views/shelf.ts       书架（目录树 / 筛选 / 导入 / 拖拽）
├─ views/reader.ts      阅读页（翻页 / 目录 / 搜索 / 设置）
└─ styles/app.css       全部样式（设计令牌在 :root）
```

脚本：`scripts/publish.mjs`（发布）、`make_test_epub.py`（EPUB2/3 测试文件）、
`make_icon.py`（生成图标）、`make_demo_novel.py`（示例小说）。

---

## 3. 七条不变量

违反任何一条，整个架构就会塌。

1. **零副本** —— 书架只存绝对路径，永远读原文件。任何"为了性能把书转码 / 复制一份"的想法都要拒绝。
   章节索引、EPUB 解析缓存这类**纯元数据**可以落库，正文不行。
   **唯一例外：EPUB 搜索纯文本缓存** —— 存在 `<数据目录>\search-cache`，只有纯文本、只给搜索用、
   按 EPUB 的 `size + 内容指纹 + 章节数` 失效（指纹取头尾各 4KB），整个目录可删，
   **永不走显示路径**（`read_chapter` 仍读原文件）。加缓存前先想清楚范围。
2. **偏移即索引** —— 章节存 `(start_byte, end_byte)`，打开书是查表，不是扫文件。
   不要写"打开书时重新切分"的逻辑，那会从 50ms 退化到 400ms。
3. **进度是 `(chapter_index, char_offset)`，不是页码** —— 页码随字号 / 窗口变化，存了必错位。
   书架百分比是**展示层**的事：按 `chapter.char_len` 字数加权；老库没有 `char_len` 时按章节数估，
   `backfill_char_lens` 补一次就准。
4. **`novel-core` 不许依赖 UI** —— 它是唯一能脱离 Tauri 跑的东西，`novel-cli` 靠这个做压测。
5. **前端不用框架、不引 VDOM** —— 正文渲染是 `textContent` 一次写入，不经过任何 diff。
6. **删除只删记录** —— `delete_book` / `delete_folder` 只动 SQLite，**绝不 `fs::remove`**。
7. **用户数据只有一个落点** —— 书库、进度、偏好、排序、窗口状态全在 `library.db`（`setting` 表）；
   搬走这一个文件就能恢复。EPUB 搜索缓存是派生数据，可删。不要再把设置写进 localStorage。

---

## 4. 数据流

**导入**：`pickEbookFiles()` 或拖拽 → `import_paths`（async + 阻塞线程池）
→ `already_imported`（锁内只查一条索引，重复文件到此为止）
→ `prepare_import`（**锁外**：打开文档 + 探测编码 + 切章 + 解一遍全本算每章 `char_len`）
→ `commit_import`（**锁内**一次事务写 `book` + `chapter`，文档进 LRU 缓存）。
每本 `emit("import-progress")`；取消置 `AppState.import_cancel`（`AtomicBool`），
线程在**下一本开始前**退出，当前这本照常写完，已导入的不回滚。

**导入文件夹**：`import_folder(path, folder_id)`（同样 async + 阻塞线程池）
→ `scan::scan_ebooks` 递归收集 txt / epub（跳过隐藏目录、符号链接、`$RECYCLE.BIN`、`~$` 临时文件）
→ 每个文件按相对目录逐级 `ensure_folder`（**同级同名复用**，重复导入只合并、不堆重复分组）
→ 再走与 `import_paths` 相同的 `import_one`。目录最多 5 级，超出的层级拍平到最后一级；
扫描完先 emit 一条 `0/N` 进度。`ensure_folder` 只合并**同一父级**下的同名目录。

**书架字段**：`「第 N 章」来自 BookSummary.last_chapter`（0 基 `chapter_idx`），
不是拿百分比乘回去估的；`progress` 是字数加权百分比，两者不能换算。
`total_chars` 是 `chapter.char_len` 求和（`list_books` 已有子查询），老库为 0 时前端不显示。
默认排序「最近活动」用 `COALESCE(progress.updated_at, added_at)`，所以 `added_at` 一并返回。

**阅读**：`list_chapters`（查表）→ `read_chapter`（mmap seek + 解码单章）→ `Paginator.load`
→ `reflow()` 算总页数 → `goToPage`。存进度：`charAtPageStart(page)` → `save_progress`（防抖 600ms）。

**搜索**：`search` → 查询串编码成文件字节 → `memmem` 全文件扫描 → 命中换算成
`(章节, 章内字符偏移)` → 前端 `rangeOf()` 生成 Range → **CSS Custom Highlight** 画高亮（不插 DOM）。
TXT 侧维护只前进的「字节 → 字符」游标：既把 `char_offset` 从 O(章长×命中数) 降为整章一遍，
也排除 GB18030 / Big5 的**跨字符假命中**（命中点不在字符边界就丢掉）。
`search_paged(q, limit, offset)` 支持结果分页；`total` 始终是全书精确总数，前端一页 200 条 +「加载更多」。
EPUB 另走 `texts_for_search()`：首次搜索读 `<数据目录>/search-cache`，没有就逐章解压并落盘；
缓存只服务搜索，单章读失败时不落盘，避免空章被固化。

**结果行摘要契约**：每条 hit 的 `snippet` 必须**围绕本次命中**生成（不能用 `line.find(q)`，
同一行多次命中会让所有结果行显示同一段），长度 ≤40 字、命中词落在开头 10 字内。
这是和前端 `.hit-snippet` 两行 `-webkit-line-clamp` 的硬契约。常量在 `search.rs` 的 `SNIPPET_*`，
TXT 走字符版 `snippet_around`、EPUB 走 UTF-8 字节窗口版 `snippet_utf8`，`ipc.ts` 的 mock 同步。

**设置**：阅读偏好、书架排序、窗口状态走 `setting` 表（key/value JSON），由 `core/settings.ts`
经 IPC 读写；旧 localStorage 只在首次升级时迁移一次。

**数据模型（SQLite）**：

- `folder(id, parent_id, name, sort, created_at)`：目录树，深度 ≤5 由应用层校验
- `book(id, folder_id, title, author, format, file_path UNIQUE, file_size, file_mtime,
  encoding, chapter_count, cover_path, missing, added_at)`：`file_path` 是零副本锚点
- `chapter(book_id, idx, title, start, end, char_len)`：`start/end` 是原文件字节偏移，
  `char_len` 用于字数加权进度与书卡总字数（老库 0 值由 `backfill` 补）
- `progress(book_id, chapter_idx, char_offset, updated_at)`：不存页码
- `setting(key, value)`：偏好 / 窗口状态 / 其他轻量设置

**文件失效**：读取前 stat；文件没了标 `missing`（保留记录），回来恢复；mtime/size 变了重建章节索引
并 `emit("book-reindexed")`，绝不改写原文件。

**作者解析**：TXT 先读文件头 4KB、前 30 行匹配 `(?:作者|著者)\s*[:：]\s*(1–20 字)`；
没有就拆文件名（只认 `-` `－` `—`，左边 ≤12 字才当作者）。EPUB 只读 OPF 的 `dc:creator`（取第一个）。
两边都没有就是 `None`，书架显示「未知作者」。已知误判：`书名-完本.txt` 会把「书名」当作者。

---

## 5. 回归基线

数据：`novel-cli bench`，30MB GB18030 合成语料（`novel-cli gen samples/bench30.txt 30` 可重新生成）。

| 指标 | 红线 | 实测 |
|---|---|---|
| 打开 + 建目录（30MB） | < 400 ms | 59.7 ms |
| 随机读一章 | < 30 ms | 0.011 ms |
| 全文搜索（11.8 万命中，含精确计数） | < 120 ms | 3.99 ms |
| 真实书（10.8MB / 1516 章）打开 | — | 24 ms |

数据层：`selftest` **39 项全过**（目录深度 / 零副本 / 进度持久化 / 失效恢复 / 同级排序 /
字数加权 / 同名目录合并）。引擎测试：`cargo test -p novel-core` **56 项全过**
（编码 / 切章 / 搜索摘要 / 数据层 / EPUB / 搜索缓存 / 字体枚举 / 文件夹扫描）。

EPUB 搜索（9.61MB / 200 章 / 700 万字）：冷缓存首次 182ms → 内存缓存 2.2–2.9ms（红线 10）；
重启后首次命中磁盘缓存约 25ms。30MB 首次导入（含算字数）约 0.31s；老库回填 66 本约 2.0s（一次性）。

规模基线：66 本 · 54,002 章 · 库体积 2.7MB · GB18030 57 本 + UTF-8 9 本 · 作者命中 61/66。
导入文件夹实测（真实 exe + 临时库）：4 本 → 3 个分组，二次导入 `foldersCreated=0`、4 本全部去重。
前端产物：主包 JS 71.0KB（gzip 26.0KB）+ CSS 29.0KB（gzip 6.5KB）；
拖拽相关的 `webview` 分片 16.5KB 按需加载。没有引入任何运行时依赖。

---

## 6. 已知问题与验证欠账

**当前没有已知的未修复缺陷。** 几个容易再踩的设计点：

- 左右 30% 热区里的双击选词会让位给翻页（见 §1.3 坑 6），拖选和中间 40% 不受影响。
- 作者解析的 12 字阈值会把 `书名-完本` 拆错（见 §4 末尾），目前没有手动编辑作者的入口。
- EPUB 的 `dc:creator` 不清洗「著 / 译 / 多作者」，原样入库。

**已验证**：66 本真实库全量导入、进度加权、文件失效恢复；导入文件夹（真实 exe + 临时数据目录）；
阅读页点击翻页 / 圆形按钮 / 搜索分组 / 设置恢复默认 / 边距 1px（CDP 实测）。

**还没验**：

- 窗口记忆的多显示器 / 最大化 / 最小化恢复（普通尺寸的关闭保存与启动恢复已验）。
- 导入字体的真实读盘 + `FontFace` 注册（系统字体选择已验）。
- 100MB 级单文件（目前最大 30MB）。

---

## 7. 未做与暂缓

- **翻页动画**：不做。仿真翻页要 canvas + 3D，属于重写渲染层，`applyTransform()` 是唯一入口，代价大收益小。
- **双列 / 竖排**：不做。实测 `writing-mode: vertical-rl` 下 CSS 多栏是**沿纵向堆叠**的
  （600×300 容器 `scrollHeight` 3700 / `scrollWidth` 600），"下一页"会往下走，不是右→左翻。
  真要做右→左，得放弃多栏，改成「定尺寸竖向流 + 横向平移窗口」，等于重写 `reflow` / `columnOf` / `applyTransform`。
  想凑"竖着读"的观感，把边距拉满、正文挤成约 120px 窄竖列即可。
- **按行滚动 / 纵向连续流**：暂缓。当前布局垂直方向没有可滚余量，`translateY` 只会露空白。
  真要做走「`overflow-y: auto` + `scrollTop`」，需重写分页核心、底栏改百分比；
  但进度格式天然兼容，`novel-core` 全不动。目前只把 `↑` `↓` 绑到整页翻页。
- **封面提取**：不做。书卡以文件名 + 作者 + 章节 · 进度展示，`cover_path` 保留不用。
- **跨父级拖拽排序**：没做。只能同级拖，跨级用右键「移动到…」。
- **自动更新**：不做（个人项目）。
- **100MB 压测**：验证欠账，不是功能（见 §6）。

---

## 8. 代码约定

### 加一个 Tauri 命令（四个地方，缺一不可）

1. `crates/novel-core/src/` —— 业务逻辑写引擎里，**不要写在 Tauri 层**
2. `src-tauri/src/lib.rs` —— `#[tauri::command]` 薄包装 + 加进 `generate_handler![]`
3. `src/core/ipc.ts` —— 加 TS 类型 + `api.xxx` 方法（参数用 camelCase，Tauri 自动转 snake_case）
4. 同文件 `mock()` 分支 —— 要么实现，要么 `Promise.reject(new Error("浏览器 preview 模式未实现：xxx"))`

### 加一个界面

`src/views/xxx.ts` 导出 `XxxView(...): HTMLElement`，在 `app.ts` 里 `root.replaceChildren(...)` 切换。
用 `h()` 构造、`signal/effect` 驱动，**不要直接 `innerHTML`**（`html` 属性仅限可信内容）。

### 样式

全部在 `src/styles/app.css`，按块注释分区。颜色 / 圆角 / 字体用 `:root` 的设计令牌；
阅读页另有一套 `--reader-*` 变量在 `.reader` 上，四套主题靠 `[data-theme]` 覆盖
（`reader/prefs.ts::THEME_LABELS`，`sepia` 显示为「羊皮纸」）。羊皮纸纹路是三层
`background-image`（暗角 + `feTurbulence` 云斑 + 颗粒），**不新增图片**，必须挂背景层而不是伪元素，
否则会盖住正文选中或改高度触发重排。

滚动条用原生样式，不要加全局 `::-webkit-scrollbar` 覆写。底栏章节进度是自绘 range：
`input` 的 `color` 默认不继承，必须写 `color: inherit`；已读段末端 `--fill` 按圆点中心算成 **px**；
圆点宽度 `SLIDER_THUMB` 与 hover 预览共用，改尺寸要两处同步。

### 右键菜单

WebView2 原生右键菜单在 `app.ts` 里全局关掉，只给文本输入放行（右键粘贴还得靠它）。
需要菜单的地方一律用 `core/menu.ts` 的 `showContextMenu` 自绘（书卡、目录、正文选中「复制」）。

### 错误处理

Rust 侧 `thiserror` 定义错误，Tauri 层 `.map_err(|e| e.to_string())` 转字符串。
前端捕获后**必须给用户可见反馈**（Toast 或标题文字），不要只 `console.error`。

---

## 9. 调试与验证

| 场景 | 手段 |
|---|---|
| 调界面（不启 Tauri） | `pnpm dev` → `http://localhost:1420`，mock 提供 8 本书 + 程序化正文 |
| 验证切分 / 编码 | `novel-cli info / toc / read <文件>` |
| 验证性能 | `novel-cli bench <文件> [关键词]` |
| 验证数据层 | `novel-cli selftest` |
| 直接改书架数据 | `novel-cli lib-import / lib-folder / lib-books <db>` |
| 给老库补章节字数 | `novel-cli backfill <db>`（启动时也会自动补） |
| 造测试书 | `novel-cli gen <输出> [MB]`、`make_test_epub.py`、`make_demo_novel.py` |
| 看库里明细 | Python 内置 sqlite3 查 `%APPDATA%\com.novelreader.app\library.db` |

`samples/` 下不留大文件，需要时现场生成。

### 界面验证

- 浏览器 mock：`pnpm dev`，用 `localhost` 不要 `127.0.0.1`。
- 真实 Tauri：设 `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9224` 启动，
  CDP 连 `http://127.0.0.1:9224/json`；`Runtime.evaluate` 读 DOM、`Page.captureScreenshot` 截图，
  可直接 `__TAURI_INTERNALS__.invoke(...)` 绕过文件对话框。
  想不污染真实书架，用 `NOVEL_READER_DATA_DIR` 指向临时目录启动。
- headless Chrome：`chrome --headless=new --remote-debugging-port=9222 --user-data-dir="$TEMP/cdp" --window-size=1280,860 --disable-gpu http://localhost:1420/`。
- 拖拽 / 文本选择必须用 `Input.dispatchMouseEvent` 真实鼠标事件（选择前先 `Page.bringToFront`）。

---

## 10. 发布

```
release/NovelReader.exe          # 唯一分发产物，免安装，双击即用
release/SHA256SUMS.txt           # 绿色版校验和
target/release/novel-reader.exe  # 未改名的原始产物
```

`bundle.active=false`，**只分发免安装绿色版，不生成 NSIS 安装包**（`target/release/bundle/`
不再产生）。`release/` 在 `.gitignore` 里。`pnpm tauri build` 只写 `target/`，出包后跑
**`pnpm release`**（`scripts/publish.mjs`）同步：以 `tauri.conf.json` 的 version 为单一源校验
另外三处、检测 `NovelReader.exe` 占用后安全替换（不结束用户进程）、生成
`release/SHA256SUMS.txt`。Windows 上正在运行的 exe 可以改名但不能覆盖写，脚本靠
「改名 → 写新文件」绕过文件锁；极端情况会留下 `*.exe.old`，关掉程序后可手动删。

绿色版 exe **未做代码签名**：用户首次下载运行会看到 SmartScreen 提示，需在 Release
说明里写明「更多信息 → 仍要运行」并提供校验和。卸载 = 删 exe，书架数据仍在
`%APPDATA%\com.novelreader.app\library.db`，需要手动删。

- `app.security.csp` 是显式最小 CSP（本地应用 XSS 面小，但别再退回 `null`）；
  `capabilities/default.json` 只给 `dialog:allow-open`，不给保存 / 写入对话框。
  改这两处后**必须用真实 exe 启动验证**，浏览器 preview 不加载 CSP。
- `bundle` 段现在只保留 `active: false` 与 `icon`；`icon.ico` 仍由构建期嵌入 exe，
  与是否生成安装包无关。

**发布说明**：`.github/release-notes.md` 是当前待发版本的说明模板（新增 / 优化 / 修复 / 其它 +
固定「下载与运行」段）。workflow 用 `gh release create --notes-file` 读它，`--generate-notes`
再在后面追加 PR 列表与 compare 链接。GitHub 自动说明**只基于合并的 PR 和 label，不看 commit**，
本项目没有 PR，所以分类内容必须手写；不要指望 `--generate-notes` 生成「新增 / 优化」。
发版顺序：写说明 → `pnpm bump` → commit → push → 打 tag。草稿生成后还能在网页继续编辑，
确认无误再 Publish。文件里留着 `TODO_REPLACE_BEFORE_RELEASE` 时 CI 只发警告、不阻断。
说明取自 **tag 指向的那个 commit**，所以必须和版本号一起提交。
- **不要为了消掉 `identifier` 以 `.app` 结尾的警告去改它**。数据库落在
  `%APPDATA%\com.novelreader.app\`，改 identifier 等于换了一个应用，现有书架会整个"消失"。
- 用户数据默认在 `%APPDATA%\com.novelreader.app\`；自定义目录的指针是 `data-dir.txt`。
  迁移走书架「设置 → 数据目录」，不要手工改指针或覆盖正在用的 db。

### 禁止事项（会出事故，不是风格）

| ❌ | 后果 |
|---|---|
| 改 `identifier` | 书库路径变了，用户书架"消失" |
| 关 `dragDropEnabled` | 丢拖拽导入，Windows 上 HTML5 拖拽也一起废 |
| `import_paths` / `import_folder` 改同步 | 批量导入堵死消息循环 |
| 持 `Mutex<Library>` 做全本解码 | 界面卡顿；解码放锁外（`prepare_import` / `backfill_char_lens`） |
| `fs::remove` / 改写原文件 | 违反零副本 |
| 给 `novel-core` 引 UI / 重依赖 | 必须能脱离 Tauri 压测 |
| regex 反向引用 | 运行时 panic |
| 手工改 `data-dir.txt` / 覆盖正在用的 db | 应用可能打不开或丢设置 |
