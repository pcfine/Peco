// 列表容器：空态 / 骨架 / 错误 / 行，以及「同一时刻只展开一段历史」。

import { useState } from "react";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { ScheduleRow } from "@/components/schedules/ScheduleRow";
import { errorText } from "@/lib/api";
import type { ScheduleView } from "@/types/daemon";

/** 空态要说清「这东西到底做什么」，否则新建按钮摆在那里没人敢点。 */
export const SCHEDULES_EMPTY_HINT =
  "定时任务按 cron 把一段话投进指定空间的 @master 会话，和你在那儿发一条消息一样。";

interface ScheduleListProps {
  schedules: ScheduleView[];
  isPending: boolean;
  error: Error | null;
  onRetry: () => void;
  onEdit: (schedule: ScheduleView) => void;
  onDelete: (schedule: ScheduleView) => void;
}

export function ScheduleList({
  schedules,
  isPending,
  error,
  onRetry,
  onEdit,
  onDelete,
}: ScheduleListProps) {
  // 展开一段就收起上一段：同时铺开几条历史会把列表挤出屏。
  const [expandedId, setExpandedId] = useState<string | null>(null);

  if (isPending) {
    return (
      <div className="space-y-px">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-11 w-full" />
        ))}
      </div>
    );
  }

  return (
    <div className="space-y-3">
      {error && <ErrorBanner message={errorText(error)} onRetry={onRetry} />}
      {schedules.length === 0 && !error && (
        <EmptyState title={SCHEDULES_EMPTY_HINT} />
      )}
      {schedules.length > 0 && (
        <ul>
          {schedules.map((schedule) => (
            <ScheduleRow
              key={schedule.id}
              schedule={schedule}
              expanded={expandedId === schedule.id}
              onToggleHistory={() =>
                setExpandedId(expandedId === schedule.id ? null : schedule.id)
              }
              onEdit={() => onEdit(schedule)}
              onDelete={() => onDelete(schedule)}
            />
          ))}
        </ul>
      )}
    </div>
  );
}
