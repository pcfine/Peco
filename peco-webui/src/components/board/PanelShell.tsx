// 面板的统一外壳：h-28 头部 + 12 内边距，无边框、无圆角、无阴影 ——
// 相邻面板的分隔线由看板栅格的 1px gap 提供。

import type { ReactNode } from "react";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { errorText } from "@/lib/api";
import { cn } from "@/lib/utils";

interface PanelShellProps {
  title: string;
  action?: ReactNode;
  isPending?: boolean;
  /** 重取中：标题右侧给一句静态「更新中」，不闪不脉冲。 */
  isFetching?: boolean;
  error?: unknown;
  onRetry?: () => void;
  /** 有旧数据时错误只是降透明度，不整块替换。 */
  hasData?: boolean;
  skeletonRows?: number;
  children: ReactNode;
}

export function PanelShell({
  title,
  action,
  isPending = false,
  isFetching = false,
  error,
  onRetry,
  hasData = false,
  skeletonRows = 3,
  children,
}: PanelShellProps) {
  return (
    <section className="flex min-w-0 flex-col bg-panel">
      <div className="flex h-7 shrink-0 items-center gap-2 pl-3 pr-1.5">
        <h2 className="text-sm font-semibold text-ink">{title}</h2>
        {isFetching && !isPending && (
          <span
            className="text-2xs text-ink-faint"
            data-testid="panel-fetching"
          >
            更新中
          </span>
        )}
        <div className="ml-auto flex items-center gap-1">{action}</div>
      </div>
      <div className="min-w-0 flex-1 p-3 pt-0">
        {error ? (
          <div className="mb-2">
            <ErrorBanner message={errorText(error)} onRetry={onRetry} />
          </div>
        ) : null}
        {isPending ? (
          <div className="space-y-px">
            {Array.from({ length: skeletonRows }, (_, row) => (
              <Skeleton key={row} className="h-8 w-full" />
            ))}
          </div>
        ) : (
          <div className={cn(Boolean(error) && hasData && "opacity-60")}>
            {children}
          </div>
        )}
      </div>
    </section>
  );
}
