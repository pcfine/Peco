// TaskHub 快照的展示翻译层：字节窗口 → 文本、进度 → 一句话、在飞计数。
//
// 快照里的 `output.tail` 是原始字节而不是字符串（Hub 按字节裁剪窗口），解码必须
// 容忍首尾落在多字节字符中间，否则用户看到的第一屏是一片替换符。

import { isTerminalTaskState } from "@/lib/taskStates";
import type {
  LiveTaskProgress,
  LiveTaskView,
  TaggedJson,
} from "@/types/daemon";

/** 展开区最多渲染这么多字符：窗口本身有 64 KB，整段进 DOM 每 3 秒重排一次。 */
const MAX_TAIL_CHARS = 4000;

function isContinuationByte(byte: number): boolean {
  return (byte & 0xc0) === 0x80;
}

/** 首字节声明的序列宽度；非法首字节当单字节处理，交给解码器出替换符。 */
function sequenceWidth(byte: number): number {
  if (byte < 0x80) return 1;
  if ((byte & 0xe0) === 0xc0) return 2;
  if ((byte & 0xf0) === 0xe0) return 3;
  if ((byte & 0xf8) === 0xf0) return 4;
  return 1;
}

/** 尾部最后一个字符只到了 `m < n` 个字节：这 `m` 个字节不能解码。 */
function trailingIncompleteBytes(
  bytes: Uint8Array,
  start: number,
  end: number,
): number {
  for (let back = 1; back <= 4; back++) {
    const index = end - back;
    if (index < start) return 0;
    const byte = bytes[index];
    if (isContinuationByte(byte)) continue;
    return back < sequenceWidth(byte) ? back : 0;
  }
  return 0;
}

/**
 * 输出窗口的字节 → 文本。
 *
 * 开头被裁掉的那个字符只剩续接字节，结尾也可能只写了半个字符：两端都切掉，
 * 而不是解码出替换符。
 */
export function decodeOutputTail(tail: readonly number[]): string {
  const bytes = Uint8Array.from(tail);
  let start = 0;
  while (start < bytes.length && isContinuationByte(bytes[start])) start++;
  const end =
    bytes.length - trailingIncompleteBytes(bytes, start, bytes.length);
  if (end <= start) return "";
  return new TextDecoder("utf-8").decode(bytes.subarray(start, end));
}

/** 展示用的输出尾巴：超长只留末尾 —— 用户盯的是最新那段。 */
export function tailTextForDisplay(tail: readonly number[]): {
  text: string;
  truncated: boolean;
} {
  const text = decodeOutputTail(tail);
  if (text.length <= MAX_TAIL_CHARS) return { text, truncated: false };
  return { text: text.slice(text.length - MAX_TAIL_CHARS), truncated: true };
}

/** 进度一句话：`生成中 3 / 10`。三格全空返回 `null`，由调用方整段省略。 */
export function progressLabel(
  progress: LiveTaskProgress | null | undefined,
): string | null {
  if (!progress) return null;
  const phase = progress.phase?.trim() || null;
  const { current, total } = progress;
  const counts =
    current === null
      ? null
      : total === null
        ? String(current)
        : `${current} / ${total}`;
  if (phase && counts) return `${phase} ${counts}`;
  return phase ?? counts;
}

/**
 * 耗时：终态按 `updated_at - created_at`（终态收敛时刷新 `updated_at`），
 * 在飞按「到现在」。时间串读不出来返回 `null`，由调用方显示占位符。
 */
export function liveTaskElapsedMs(
  task: LiveTaskView,
  now: number = Date.now(),
): number | null {
  const start = Date.parse(task.created_at);
  if (Number.isNaN(start)) return null;
  const end = isTerminalTaskState(task.state)
    ? Date.parse(task.updated_at)
    : now;
  if (Number.isNaN(end)) return null;
  return Math.max(0, end - start);
}

/** 在飞在前、同组按创建时间倒序：Hub 槽常驻，已完成的任务会一直堆在同一份列表里。 */
export function sortLiveTasks(tasks: readonly LiveTaskView[]): LiveTaskView[] {
  return [...tasks].sort((left, right) => {
    const leftEnded = isTerminalTaskState(left.state);
    const rightEnded = isTerminalTaskState(right.state);
    if (leftEnded !== rightEnded) return leftEnded ? 1 : -1;
    return Date.parse(right.created_at) - Date.parse(left.created_at);
  });
}

/** 在飞 = 非终态。还没拿到数据时返回 `null`，让调用方决定回落到哪一份计数。 */
export function liveInflightCount(
  tasks: readonly LiveTaskView[] | undefined,
): number | null {
  if (!tasks) return null;
  return tasks.filter((task) => !isTerminalTaskState(task.state)).length;
}

/** 终态完整输出：`payload` 由执行体定义，字符串直接读，其余按缩进 JSON 展示。 */
export function outcomeOutputText(
  output: TaggedJson | null | undefined,
): string | null {
  if (!output) return null;
  if (typeof output.payload === "string") return output.payload;
  if (output.payload === null || output.payload === undefined) return null;
  return JSON.stringify(output.payload, null, 2);
}
