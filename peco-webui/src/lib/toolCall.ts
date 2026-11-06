// 工具调用卡片的视图模型与折叠算法共用件 —— 历史快照与实时流走同一套语义，
// 两条路径只在「数据从哪来」上不同。

export type ToolCallStatus = "pending" | "done" | "missing";

export interface ToolCallView {
  id: string;
  name: string;
  arguments: string;
  /** 仅在 status === "done" 时读取 —— `status` 是唯一权威，result 不参与状态判断。 */
  result?: string;
  status: ToolCallStatus;
  /** 输出来自孤儿 tool 消息（找不到配对调用），归属合成宿主而非真实 assistant。 */
  orphan?: boolean;
  /** 工具输出图片（data URI）。实时侧由 SSE 下发，历史侧后端恒空。 */
  images?: string[];
}

export const OUTPUT_CHAR_CAP = 20_000;
export const ARGUMENTS_CHAR_CAP = 8_000;
export const SUMMARY_CHAR_CAP = 80;

export interface CappedText {
  text: string;
  truncated: boolean;
  total: number;
}

/** 超限截断并标记；`total` 保留原始字符数，供「共 N 字符」提示使用。 */
export function capText(s: string, cap: number): CappedText {
  if (s.length <= cap) return { text: s, truncated: false, total: s.length };
  return { text: `${s.slice(0, cap)}…`, truncated: true, total: s.length };
}

/**
 * 摘要候选键，按辨识度排序。`id` 不参与：它由模型自造，对人类读者无信息量。
 */
const SUMMARY_KEYS = [
  "command",
  "cmd",
  "query",
  "q",
  "url",
  "path",
  "file_path",
  "file",
  "pattern",
  "name",
  "agent_name",
  "prompt",
];

export function flattenWhitespace(s: string): string {
  return s.replace(/\s+/g, " ").trim();
}

/**
 * 折叠行摘要：取参数里最具辨识度的那个值。
 *
 * 全部候选键未命中时返回空串，不兜底取任意值 —— 摘要既进整行显示也进
 * aria-label，猜错（如把 `{"id":"abc"}` 显示成 abc）比留空更误导。
 *
 * 非对象 JSON 无键可挑，退回去空白化后的原串：原串仍是最贴近工具意图的描述。
 */
export function summarizeArguments(argsJson: string): string {
  let parsed: unknown;
  try {
    parsed = JSON.parse(argsJson);
  } catch {
    return capText(flattenWhitespace(argsJson), SUMMARY_CHAR_CAP).text;
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    return capText(flattenWhitespace(argsJson), SUMMARY_CHAR_CAP).text;
  }

  const record = parsed as Record<string, unknown>;
  for (const key of SUMMARY_KEYS) {
    const value = record[key];
    const kind = typeof value;
    // null / 对象 / 数组跳过并继续 —— 它们展开成 "null" 或 "[object Object]" 只会污染摘要
    if (kind !== "string" && kind !== "number" && kind !== "boolean") continue;
    const text = flattenWhitespace(String(value));
    if (text !== "") return capText(text, SUMMARY_CHAR_CAP).text;
  }
  return "";
}

/**
 * 轮末收尾：残留 pending → missing。
 *
 * 必须扫全部消息 —— 错误气泡、子 Agent 卡片都追加在末尾，持有 pending 的
 * assistant 不一定是末条。无 pending 时返回原引用，避免无谓重渲染。
 */
export function settlePendingToolCalls<
  T extends { toolCalls?: ToolCallView[] },
>(messages: T[]): T[] {
  let settled = false;
  const next = messages.map((m) => {
    if (!m.toolCalls?.some((tc) => tc.status === "pending")) return m;
    settled = true;
    return {
      ...m,
      toolCalls: m.toolCalls.map((tc) =>
        tc.status === "pending" ? { ...tc, status: "missing" as const } : tc,
      ),
    };
  });
  return settled ? next : messages;
}
