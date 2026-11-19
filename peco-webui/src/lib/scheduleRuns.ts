// 执行历史里的跳过成因只在这里翻成人话：后端存的是稳定标识符（`previous_running`），
// 原样显示等于把内部名字丢给用户。

const SKIP_REASON_LABELS: Record<string, string> = {
  missed: "错过触发窗口，本轮未执行",
  previous_running: "上一次运行还没结束，本轮跳过",
  conversation_busy: "目标会话一直忙，本轮放弃",
  schedule_disabled: "任务已停用，本轮未执行",
};

/** 认得的成因翻成人话；中文人话与 `work_dir_missing: …` 这类带 code 的诊断原样保留。 */
export function runErrorText(error: string): string {
  return SKIP_REASON_LABELS[error] ?? error;
}
