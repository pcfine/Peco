// ============================================================================
// MemoryConsolidationCard — 记忆自动整理 opt-in 开关 + 最近一次运行状态
// ============================================================================
//
// 对应后端：
//   GET/PUT /api/peco/memory/consolidation/optin — 用户级 opt-in 意愿
//   GET     /api/peco/memory/consolidation/state — 最近一次整理的时刻与统计
//
// 开关只表达意愿：服务器总开关未放开时同样可保存，写成功即回显。
// 不提供「立即整理」入口 —— 手动触发属高级操作，本卡片不暴露以免误触。
// 不做缓存：每次进设置页都重新拉取，避免过期状态误导。

import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Separator } from "@/components/ui/separator";
import { Switch } from "@/components/ui/switch";
import {
  getConsolidationOptin,
  getConsolidationState,
  setConsolidationOptin,
  type ConsolidationRunStats,
  type ConsolidationStateResponse,
} from "@/api/peco";

/** 统计字段的展示顺序与标签。 */
const STAT_FIELDS: { key: keyof ConsolidationRunStats; label: string }[] = [
  { key: "scanned", label: "扫描" },
  { key: "candidates", label: "候选" },
  { key: "clustered_groups", label: "聚类组" },
  { key: "merged", label: "沉淀" },
  { key: "dedup_deleted", label: "去重删除" },
  { key: "ttl_deleted", label: "TTL 删除" },
  { key: "audit_purged", label: "审计清理" },
  { key: "llm_calls", label: "模型调用" },
];

/** RFC 3339 时刻 → 本地时间；无法解析时原样回显。 */
function formatRunAt(raw: string): string {
  const at = new Date(raw);
  return Number.isNaN(at.getTime()) ? raw : at.toLocaleString("zh-CN");
}

function StatValue({ value }: { value: number | null | undefined }) {
  return (
    <span className="tabular-nums">
      {typeof value === "number" ? value.toLocaleString() : "—"}
    </span>
  );
}

export function MemoryConsolidationCard() {
  const [enabled, setEnabled] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [pending, setPending] = useState(false);
  const [loadFailed, setLoadFailed] = useState(false);
  const [runState, setRunState] = useState<ConsolidationStateResponse | null>(
    null,
  );
  const [runStateFailed, setRunStateFailed] = useState(false);

  useEffect(() => {
    let alive = true;

    getConsolidationOptin()
      .then((res) => {
        if (alive) setEnabled(res.enabled);
      })
      .catch(() => {
        if (alive) setLoadFailed(true);
      })
      .finally(() => {
        if (alive) setLoaded(true);
      });

    getConsolidationState()
      .then((res) => {
        if (alive) setRunState(res);
      })
      .catch(() => {
        if (alive) setRunStateFailed(true);
      });

    return () => {
      alive = false;
    };
  }, []);

  const handleToggle = useCallback((next: boolean) => {
    setEnabled(next); // 乐观更新
    setPending(true);
    setConsolidationOptin(next)
      .then((res) => setEnabled(res.enabled))
      .catch(() => {
        setEnabled(!next); // 回滚
        toast.error("自动记忆整理开关保存失败");
      })
      .finally(() => setPending(false));
  }, []);

  const stats =
    runState?.last_run_stats && typeof runState.last_run_stats === "object"
      ? runState.last_run_stats
      : null;

  const lastRunText = runStateFailed
    ? "状态读取失败"
    : runState?.last_run_at
      ? formatRunAt(runState.last_run_at)
      : "从未运行";

  return (
    <Card>
      <CardHeader>
        <CardTitle>自动记忆整理</CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex items-start justify-between gap-4">
          <CardDescription>
            开启后系统将在你空闲时（默认每 30
            分钟检查）自动归纳整理你的长期记忆；服务器未开放时开关可保存但不生效。
          </CardDescription>
          <Switch
            aria-label="自动记忆整理"
            checked={enabled}
            disabled={!loaded || pending}
            onCheckedChange={handleToggle}
          />
        </div>
        {loadFailed && (
          <p className="text-destructive text-sm">
            开关状态读取失败，请刷新页面重试。
          </p>
        )}

        <Separator />

        <div className="space-y-3">
          <div className="flex items-center justify-between text-sm">
            <span className="text-muted-foreground">上次整理</span>
            <span className="tabular-nums">{lastRunText}</span>
          </div>

          {stats ? (
            <>
              <div className="grid grid-cols-2 gap-x-4 gap-y-3 sm:grid-cols-4">
                {STAT_FIELDS.map((field) => (
                  <div key={field.key} className="space-y-0.5">
                    <p className="text-muted-foreground text-xs">
                      {field.label}
                    </p>
                    <p className="text-sm">
                      <StatValue value={stats[field.key]} />
                    </p>
                  </div>
                ))}
              </div>
              {stats.machine_steps_skipped && (
                <p className="text-muted-foreground text-xs">
                  机器判定已跳过：{stats.machine_steps_skipped}
                </p>
              )}
            </>
          ) : (
            <div className="flex items-center justify-between text-sm">
              <span className="text-muted-foreground">上次统计</span>
              <span className="tabular-nums">—</span>
            </div>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
