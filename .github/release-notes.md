<!--
TODO_REPLACE_BEFORE_RELEASE

本文件是「当前待发版本」的 Release 说明，由 .github/workflows/release.yml 的
`gh release create --notes-file` 读取。发版前请：
  1. 把下面「本次更新」四节替换成本次真实变更（没有的节直接删掉）；
  2. 删掉文件开头这行 TODO_REPLACE_BEFORE_RELEASE（否则 CI 会发一条警告，但不阻断）；
  3. 和版本号改动一起 commit，再打 tag —— notes 取自 tag 指向的那个 commit。

「下载与运行」是固定段，一般不用改。
-->

## 新增

- （本次新增的功能，一条一行；没有就删掉这一节）

## 优化

- （体验、性能、代码层面的改进）

## 修复

- （修掉的问题，可带 issue 编号）

## 其它

- （版本号、依赖升级、文档等）

---

## 下载与运行

只提供免安装的 `NovelReader.exe`，下载后放到任意目录双击即可，无需安装、不写注册表。

- 首次运行可能出现 Windows SmartScreen 的「Windows 已保护你的电脑」提示，点 **更多信息 → 仍要运行**。可用 `SHA256SUMS.txt` 核对文件完整性。
- 卸载只需删除 exe；书架、进度与偏好保存在 `%APPDATA%\com.novelreader.app\library.db`，不会随 exe 删除。
- 未做自动更新，升级就是重新下载 exe 覆盖原文件（覆盖前建议先退出程序）。
