// 暂停 / 恢复命令的失败处理：两者都与「轮还在不在」赛跑。

import { toast } from "sonner";

/**
 * `409 no_running_task` = 这一轮在我们按下之前已经收尾，不是故障。
 *
 * 其余错误才值得说一句 —— 静默吞掉会让「按了没反应」无迹可循。两个动作共用
 * 这里，文案要带上是哪一下，否则用户判不出哪个按钮没生效。
 */
export function ignoreNoRunningTask(
  err: unknown,
  action: "暂停" | "恢复",
): void {
  if ((err as { code?: string } | null)?.code === "no_running_task") return;
  toast.error(`${action}失败，请重试`);
}
