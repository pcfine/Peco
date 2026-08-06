import type { StatisticsResponse } from "@/types/workflow";
import { Card, CardContent } from "@/components/ui/card";
import { cn } from "@/lib/utils";

interface StatisticsPanelProps {
  stats: StatisticsResponse | null;
  loading?: boolean;
  className?: string;
}

function formatDuration(ms: number): string {
  if (ms === 0) return "—";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  return `${Math.floor(ms / 60000)}m ${Math.round((ms % 60000) / 1000)}s`;
}

function StatCard({
  label,
  value,
  sub,
}: {
  label: string;
  value: string;
  sub?: string;
}) {
  return (
    <Card>
      <CardContent className="p-4 text-center">
        <p className="text-2xl font-bold">{value}</p>
        <p className="text-xs text-muted-foreground mt-1">{label}</p>
        {sub && <p className="text-xs text-muted-foreground">{sub}</p>}
      </CardContent>
    </Card>
  );
}

/**
 * Phase 1 statistics panel — simple cards + bar chart using pure CSS.
 * Phase 2 target: Recharts BarChart for run history and step distribution.
 */
export function StatisticsPanel({
  stats,
  loading,
  className,
}: StatisticsPanelProps) {
  if (loading) {
    return (
      <div
        className={cn(
          "text-sm text-muted-foreground py-8 text-center",
          className,
        )}
      >
        加载统计数据...
      </div>
    );
  }

  if (!stats) {
    return (
      <div
        className={cn(
          "text-sm text-muted-foreground py-8 text-center",
          className,
        )}
      >
        暂无统计数据
      </div>
    );
  }

  const successRatePct = (stats.successRate * 100).toFixed(1);

  return (
    <div className={cn("space-y-6", className)}>
      {/* Top-level stat cards */}
      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <StatCard label="总运行次数" value={String(stats.totalRuns)} />
        <StatCard
          label="成功率"
          value={`${successRatePct}%`}
          sub={`${stats.successCount} 成功 / ${stats.failureCount} 失败`}
        />
        <StatCard
          label="平均耗时"
          value={formatDuration(stats.avgDurationMs)}
        />
        <StatCard label="已取消" value={String(stats.cancelledCount)} />
      </div>

      {/* 30-day run history — simple bar chart */}
      {stats.runHistory30d.length > 0 && (
        <div>
          <h4 className="text-sm font-medium mb-3">30 天运行趋势</h4>
          <div className="flex items-end gap-0.5 h-24">
            {stats.runHistory30d.map((day) => {
              const maxTotal = Math.max(
                ...stats.runHistory30d.map((d) => d.total),
                1,
              );
              const totalH = (day.total / maxTotal) * 100;
              const successH =
                day.total > 0 ? (day.success / day.total) * totalH : 0;
              const failureH = totalH - successH;

              return (
                <div
                  key={day.date}
                  className="flex-1 flex flex-col justify-end group relative"
                  title={`${day.date}: ${day.success} 成功 / ${day.failure} 失败`}
                >
                  <div
                    style={{ height: `${totalH}%` }}
                    className="w-full flex flex-col justify-end"
                  >
                    {failureH > 0 && (
                      <div
                        style={{ height: `${(failureH / totalH) * 100}%` }}
                        className="w-full bg-red-400 rounded-t-sm min-h-[2px]"
                      />
                    )}
                    {successH > 0 && (
                      <div
                        style={{ height: `${(successH / totalH) * 100}%` }}
                        className="w-full bg-green-500 rounded-t-sm min-h-[2px]"
                      />
                    )}
                  </div>
                </div>
              );
            })}
          </div>
          <div className="flex items-center gap-4 mt-2 text-xs text-muted-foreground">
            <span className="flex items-center gap-1">
              <span className="w-3 h-3 bg-green-500 rounded-sm inline-block" />{" "}
              成功
            </span>
            <span className="flex items-center gap-1">
              <span className="w-3 h-3 bg-red-400 rounded-sm inline-block" />{" "}
              失败
            </span>
          </div>
        </div>
      )}

      {/* Step-level stats */}
      {stats.stepStats.length > 0 && (
        <div>
          <h4 className="text-sm font-medium mb-3">步骤耗时分布</h4>
          <div className="space-y-2">
            {stats.stepStats.map((step) => {
              const maxDur = Math.max(
                ...stats.stepStats.map((s) => s.avgDurationMs),
                1,
              );
              const pct = (step.avgDurationMs / maxDur) * 100;
              return (
                <div key={step.stepId} className="flex items-center gap-3">
                  <span className="text-xs w-24 truncate shrink-0">
                    {step.stepName}
                  </span>
                  <div className="flex-1 h-5 bg-muted rounded-sm overflow-hidden">
                    <div
                      className="h-full bg-primary/60 rounded-sm transition-all"
                      style={{ width: `${pct}%` }}
                    />
                  </div>
                  <span className="text-xs font-mono w-16 text-right shrink-0">
                    {formatDuration(step.avgDurationMs)}
                  </span>
                  <span className="text-xs text-muted-foreground w-16 text-right shrink-0">
                    {(step.failureRate * 100).toFixed(1)}% 失败
                  </span>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
