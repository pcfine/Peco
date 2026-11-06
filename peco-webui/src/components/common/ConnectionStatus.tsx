// SSE 连接状态的可见形态：8px 圆点 + 一行 11px 文字。
// 点色是唯一的非文字信号，所以文案必须自带「第几次重试」。

import { sseDotClass, sseLabel, type SseState } from "@/lib/sseState";
import { cn } from "@/lib/utils";

export function ConnectionStatus({
  state,
  label,
  className,
}: {
  state: SseState;
  /** 「事件」/「对话」——两条流各标一条，不合并成一个「已连接」。 */
  label?: string;
  className?: string;
}) {
  return (
    <span
      className={cn("inline-flex items-center gap-1.5", className)}
      data-sse-state={state.kind}
    >
      <span
        aria-hidden="true"
        className={cn("size-2 shrink-0 rounded-full", sseDotClass(state))}
      />
      <span className="text-2xs text-ink-faint">
        {label ? `${label} · ${sseLabel(state)}` : sseLabel(state)}
      </span>
    </span>
  );
}
