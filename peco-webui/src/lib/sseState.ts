// SSE 连接状态机：退避 1s→2s→4s→8s→15s→15s，最多 6 次，
// 用尽后停在 failed 且不自动重试 —— 后台空转比一行红字更糟。
//
// 纯函数放在这里：状态文案与点色是用户唯一能看到的承诺，必须可测。

export type SseState =
  | { kind: "idle" }
  | { kind: "open" }
  | { kind: "running" }
  | { kind: "reconnecting"; attempt: number }
  | { kind: "failed" };

/** 第 n 次重试前的等待（n 从 1 起）。 */
export const RETRY_DELAYS_MS = [1000, 2000, 4000, 8000, 15000, 15000] as const;

export const MAX_RETRIES = RETRY_DELAYS_MS.length;

export function retryDelay(attempt: number): number {
  const index = Math.min(Math.max(attempt, 1), MAX_RETRIES) - 1;
  return RETRY_DELAYS_MS[index];
}

export function sseLabel(state: SseState): string {
  switch (state.kind) {
    case "idle":
      return "未连接";
    case "open":
      return "已连接";
    case "running":
      return "生成中";
    case "reconnecting":
      return `连接中断，第 ${state.attempt} 次重试（${state.attempt}/${MAX_RETRIES}）`;
    case "failed":
      return "连接失败";
  }
}

export function sseDotClass(state: SseState): string {
  switch (state.kind) {
    case "idle":
      return "bg-ink-faint";
    case "open":
      return "bg-ok";
    case "running":
      return "bg-brand motion-safe:animate-pulse";
    case "reconnecting":
      return "bg-warn";
    case "failed":
      return "bg-danger";
  }
}
