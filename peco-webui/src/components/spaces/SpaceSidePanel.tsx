// 右栏内容：总览 / 文件 / 任务 / 记忆。
//
// 文件与任务复用状态页的同一份实现 —— 聊天页右侧只是同一批数据的另一种摆法，
// 另写一套读法只会让两处的空态与错误文案慢慢分叉。

import type { UseQueryResult } from "@tanstack/react-query";
import { SpaceFilesTab } from "@/components/spaces/SpaceFilesTab";
import { SpaceMemoryTab } from "@/components/spaces/SpaceMemoryTab";
import { SpaceOverviewTab } from "@/components/spaces/SpaceOverviewTab";
import { SpaceTasksTab } from "@/components/spaces/SpaceTasksTab";
import type {
  SpacePanelMode,
  SpacePanelVariant,
} from "@/components/spaces/useSpacePanelState";
import { usePecoTodayTasks } from "@/queries/tasks";
import { useSpaceTasks } from "@/queries/spaceDetail";
import type { Task } from "@/types/daemon";

function SpaceTasksPanel({ tasks }: { tasks: UseQueryResult<Task[], Error> }) {
  return (
    <SpaceTasksTab
      tasks={tasks.data ?? []}
      isPending={tasks.isPending}
      error={tasks.error}
      onRetry={() => void tasks.refetch()}
    />
  );
}

/** 空间页的「任务」= 打给这个空间的任务。 */
function SpaceScopedTasks({ wsId }: { wsId: string }) {
  const tasks = useSpaceTasks(wsId);
  return <SpaceTasksPanel tasks={tasks} />;
}

/**
 * Peco 页的「任务」= Peco 派出去的任务。
 *
 * Peco 是派发方，打给 Peco 空间自己的任务几乎永远是空的；照搬空间页那个方向，
 * 这个右栏就成了一块只会显示「没有任务」的死面板，角标也永远是 0。
 */
function PecoDispatchTasks({ wsId }: { wsId: string }) {
  const tasks = usePecoTodayTasks(wsId);
  return <SpaceTasksPanel tasks={tasks} />;
}

interface SpaceSidePanelProps {
  wsId: string;
  mode: SpacePanelMode;
  variant?: SpacePanelVariant;
}

export function SpaceSidePanel({
  wsId,
  mode,
  variant = "space",
}: SpaceSidePanelProps) {
  return (
    <div className="h-full min-h-0">
      {mode === "overview" && <SpaceOverviewTab />}
      {mode === "files" && <SpaceFilesTab wsId={wsId} />}
      {mode === "tasks" &&
        (variant === "peco" ? (
          <PecoDispatchTasks wsId={wsId} />
        ) : (
          <SpaceScopedTasks wsId={wsId} />
        ))}
      {mode === "memory" && <SpaceMemoryTab wsId={wsId} />}
    </div>
  );
}
