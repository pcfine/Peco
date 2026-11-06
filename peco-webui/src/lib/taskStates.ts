// 任务状态的唯一定义：分组列表与分组函数放在一起，看板与台账共用一份，
// 否则「哪个状态算失败」会有多个答案，改一处就会漏另一处。

export type TaskStateGroup =
  "active" | "waiting" | "done" | "failed" | "orphan";

/** 在飞：既没有终态，也不等外部补输入。 */
export const TASK_RUNNING_STATES: readonly string[] = ["submitted", "working"];

/** 等外部补输入或授权，任务本身没在推进。 */
export const TASK_WAITING_STATES: readonly string[] = [
  "input_required",
  "auth_required",
];

export const TASK_COMPLETED_STATE = "completed";

export const TASK_FAILED_STATES: readonly string[] = [
  "failed",
  "canceled",
  "timed_out",
  "rejected",
];

/** daemon 专有：派发曾被写下，但目标空间或观察者失联，没人再报进度。 */
export const TASK_ORPHAN_STATE = "orphan";

/** 未列举的状态按「在飞」处理，比按失败处理更保守。 */
export function taskStateGroup(taskState: string): TaskStateGroup {
  if (taskState === TASK_COMPLETED_STATE) return "done";
  if (taskState === TASK_ORPHAN_STATE) return "orphan";
  if (TASK_WAITING_STATES.includes(taskState)) return "waiting";
  if (TASK_FAILED_STATES.includes(taskState)) return "failed";
  return "active";
}
