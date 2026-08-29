// ============================================================================
// ContextMetricsCard — Peco 上下文指标
// ============================================================================
//
// 展示 GET /api/peco/session 的 context_metrics：
//   - 两口径估算 token（全量 / Verbatim viewable）与对应阈值的占比
//   - pinned 摘要长度、累计压缩次数
//   - 压缩时间线（token 变化 + 摘要长度曲线的原始数据）

import { useEffect, useState } from "react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { getPecoSession } from "@/api/peco";
import type { ContextMetrics } from "@/types/chat";

/** 单条预算占比条。 */
function BudgetBar({
  label,
  used,
  budget,
}: {
  label: string;
  used: number;
  budget: number;
}) {
  const pct = budget > 0 ? Math.min(100, Math.round((used / budget) * 100)) : 0;
  return (
    <div>
      <div className="mb-1 flex items-center justify-between text-sm">
        <span>{label}</span>
        <span className="text-muted-foreground tabular-nums">
          {used.toLocaleString()} / {budget.toLocaleString()} token（{pct}%）
        </span>
      </div>
      <div className="bg-secondary h-2 w-full overflow-hidden rounded-full">
        <div
          className="bg-primary h-full rounded-full transition-all"
          style={{ width: `${pct}%` }}
        />
      </div>
    </div>
  );
}

export function ContextMetricsCard() {
  const [metrics, setMetrics] = useState<ContextMetrics | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    getPecoSession()
      .then((snap) => setMetrics(snap.context_metrics ?? null))
      .catch(() => setFailed(true));
  }, []);

  if (failed) return null;

  return (
    <Card>
      <CardHeader>
        <CardTitle>上下文指标（个人助理）</CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        {!metrics ? (
          <p className="text-muted-foreground text-sm">暂无会话数据</p>
        ) : (
          <>
            <BudgetBar
              label="全量上下文（压缩触发口径）"
              used={metrics.estimated_total_tokens}
              budget={metrics.compaction_trigger_tokens}
            />
            <BudgetBar
              label="Verbatim 保留区（预算口径）"
              used={metrics.estimated_view_tokens}
              budget={metrics.history_token_budget}
            />
            <div className="text-muted-foreground flex gap-6 text-sm">
              <span>
                累计压缩：
                <span className="text-foreground ml-1 font-medium tabular-nums">
                  {metrics.compaction_count} 次
                </span>
              </span>
              <span>
                摘要长度：
                <span className="text-foreground ml-1 font-medium tabular-nums">
                  {metrics.pinned_summary_tokens.toLocaleString()} token
                </span>
              </span>
            </div>
            {metrics.compactions.length > 0 && (
              <div>
                <p className="text-muted-foreground mb-2 text-sm">压缩时间线</p>
                <ul className="space-y-1 text-sm">
                  {metrics.compactions.map((c, i) => (
                    <li key={i} className="flex justify-between gap-4">
                      <span className="text-muted-foreground tabular-nums">
                        {c.at}
                      </span>
                      <span className="tabular-nums">
                        归档 {c.evicted_turns} 轮 ·{" "}
                        {c.tokens_before.toLocaleString()} →{" "}
                        {c.tokens_after.toLocaleString()} token · 摘要{" "}
                        {c.summary_chars} 字
                      </span>
                    </li>
                  ))}
                </ul>
              </div>
            )}
          </>
        )}
      </CardContent>
    </Card>
  );
}
