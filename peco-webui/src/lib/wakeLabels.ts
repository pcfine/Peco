// 唤醒态的文案只在这里：HTTP 给的是裸字符串，散在组件里写中文就会各处不一致。
// `delivered` 只说「已投递」——它表示已交给运行器，任务是否处理完要看 `task_state`。

import type { WakeState, WakeSkipReason } from "@/types/daemon";

const WAKE_STATE_LABELS: Record<WakeState, string> = {
  awaiting: "等待完成",
  pending: "待投递",
  delivering: "投递中",
  delivered: "已投递",
  skipped: "已跳过",
  failed: "投递失败",
};

const WAKE_SKIP_REASON_LABELS: Record<WakeSkipReason, string> = {
  not_peco_origin: "非 Peco 来源",
  peco_identity_missing: "观察者缺少 Peco 身份",
  orphaned: "运行期失联",
  space_closed: "目标空间已关闭",
  attempts_exhausted: "重试次数用尽",
};

/** 库里出现没见过的取值时仍要给出可读文案，而不是空白或 `undefined`。 */
export function wakeStateLabel(wakeState: string): string {
  const label = (WAKE_STATE_LABELS as Record<string, string | undefined>)[
    wakeState
  ];
  return label ?? `未知唤醒态（${wakeState || "空"}）`;
}

/** 历史 skipped 行没有记录成因，显示「原因未记录」而不是猜一种。 */
export function wakeSkipReasonLabel(
  reason: WakeSkipReason | string | null,
): string {
  if (reason === null || reason === "") return "原因未记录";
  const label = (WAKE_SKIP_REASON_LABELS as Record<string, string | undefined>)[
    reason
  ];
  return label ?? `未知原因（${reason}）`;
}

/** 一句完整的唤醒描述：只有 skipped 带原因，其余状态没有成因可言。 */
export function wakeDescription(
  wakeState: string,
  reason: WakeSkipReason | string | null,
): string {
  const state = wakeStateLabel(wakeState);
  return wakeState === "skipped"
    ? `${state} · ${wakeSkipReasonLabel(reason)}`
    : state;
}
