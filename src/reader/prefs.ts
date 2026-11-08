/**
 * 阅读偏好的类型与默认值。
 * 实际的读写由 `core/settings.ts` 经 Rust 存进 library.db 的 setting 表，
 * 搬迁时只需要带走一个 db 文件。
 */

export type ReaderTheme = "paper" | "white" | "sepia" | "night";

/** serif / sans 用内置字体栈；system 用电脑已安装字体；custom 用用户导入的本地字体 */
export type ReaderFont = "serif" | "sans" | "system" | "custom";

export interface ReaderPrefs {
  fontSize: number;
  lineHeight: number;
  fontFamily: ReaderFont;
  /** 系统字体族名，只在 fontFamily === "system" 时有意义 */
  systemFamily: string;
  /** 导入的字体文件绝对路径，只在 custom 时有意义 */
  fontPath: string | null;
  theme: ReaderTheme;
  /** 左右留白，会直接影响每页字数 */
  marginX: number;
  /**
   * 点击正文左右两侧翻页。默认开；关掉后左右点击不再翻页，
   * 但点中间唤出工具栏仍然生效（那不是翻页）。
   */
  clickToTurn: boolean;
}

export const DEFAULT_PREFS: ReaderPrefs = {
  fontSize: 19,
  lineHeight: 1.9,
  fontFamily: "serif",
  systemFamily: "",
  fontPath: null,
  theme: "paper",
  marginX: 88,
  clickToTurn: true,
};

/** 字号范围：设置面板的 A− / A+ 到边界要置灰，reader.ts 与这里共用 */
export const FONT_SIZE_MIN = 14;
export const FONT_SIZE_MAX = 32;

/** 行距微调：0.1 一档，1.2~3.0 共 19 档（旧版的 1.6 / 1.9 / 2.2 都在范围内） */
export const LINE_HEIGHT_MIN = 1.2;
export const LINE_HEIGHT_MAX = 3.0;
export const LINE_HEIGHT_STEP = 0.1;

/** 常用档位，点一下直接跳，省得从 1.9 一路按到 2.8 */
export const LINE_HEIGHT_PRESETS = [
  { label: "紧凑", value: 1.5 },
  { label: "标准", value: 1.9 },
  { label: "宽松", value: 2.3 },
  { label: "超宽", value: 2.8 },
] as const;

/**
 * 把任意来源的行距夹到合法档位：旧库可能存着 1.6 / 2.2，手改 db 也可能塞进
 * 1.75 这种不在档位上的值。不归一，± 步进就会在浮点毛刺上漂移，预设按钮也永远不亮。
 */
export function clampLineHeight(value: unknown): number {
  const n = typeof value === "number" && Number.isFinite(value) ? value : DEFAULT_PREFS.lineHeight;
  const clamped = Math.min(LINE_HEIGHT_MAX, Math.max(LINE_HEIGHT_MIN, n));
  const steps = Math.round(clamped / LINE_HEIGHT_STEP);
  // 19 * 0.1 在 JS 里是 1.9000000000000001，toFixed 把倍数还原成干净的一位小数
  return Number((steps * LINE_HEIGHT_STEP).toFixed(2));
}

export const THEME_LABELS: Record<ReaderTheme, string> = {
  paper: "米黄",
  white: "纯白",
  sepia: "羊皮纸",
  night: "夜间",
};
