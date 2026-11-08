/**
 * 书架排序偏好的类型与选项。
 * 实际的读写由 `core/settings.ts` 经 Rust 存进 library.db 的 setting 表，
 * 和阅读偏好一样跟着书库走。
 */

export type SortKey = "default" | "title" | "recent" | "progress" | "chapters" | "added";

/**
 * 每个选项都有自己的「自然方向」，这里只写名字，不写方向：
 * 最近活动 / 最近阅读 / 最近添加 / 阅读进度 / 章节数 = 由新到旧、由多到少；
 * 书名 = A→Z。菜单里的「倒序」把自然方向翻过来，存的是布尔值。
 */
export const SORTS: { key: SortKey; label: string }[] = [
  { key: "default", label: "最近活动" },
  { key: "added", label: "最近添加" },
  { key: "title", label: "书名" },
  { key: "recent", label: "最近阅读" },
  { key: "progress", label: "阅读进度" },
  { key: "chapters", label: "章节数" },
];
