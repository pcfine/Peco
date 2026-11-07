// 冲突提示：有任务在飞时只说一句「可能撞车」，不锁输入、不弹确认、不排队。
//
// 并行写入的风险由用户判断 —— 它是一条状态说明，不是一次拦截。

interface SpaceConflictHintProps {
  /** 在飞任务数；0 时不渲染，避免常态提示变成噪音。 */
  inflight: number;
}

export function SpaceConflictHint({ inflight }: SpaceConflictHintProps) {
  if (inflight === 0) return null;

  return (
    <p
      data-slot="space-conflict-hint"
      role="status"
      className="mb-2 flex items-center gap-1.5 text-2xs text-ink-muted"
    >
      <span
        className="size-1.5 shrink-0 rounded-full bg-warn"
        aria-hidden="true"
      />
      <span className="min-w-0">
        Peco 有 <span className="tabular-nums text-ink">{inflight}</span>{" "}
        个任务在这个空间运行，同时写入可能冲突
      </span>
    </p>
  );
}
