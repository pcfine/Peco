// 右栏内容：文件 / 任务 / 记忆。
//
// 文件与任务复用状态页的同一份实现 —— 聊天页右侧只是同一批数据的另一种摆法，
// 另写一套读法只会让两处的空态与错误文案慢慢分叉。

import { SpaceFilesTab } from "@/components/spaces/SpaceFilesTab";
import { SpaceMemoryTab } from "@/components/spaces/SpaceMemoryTab";
import { SpaceTasksTab } from "@/components/spaces/SpaceTasksTab";
import type { SpacePanelMode } from "@/components/spaces/useSpacePanelState";
import { useSpaceTasks } from "@/queries/spaceDetail";

function SpaceTasksPanel({ wsId }: { wsId: string }) {
  const tasks = useSpaceTasks(wsId);

  return (
    <SpaceTasksTab
      tasks={tasks.data ?? []}
      isPending={tasks.isPending}
      error={tasks.error}
      onRetry={() => void tasks.refetch()}
    />
  );
}

interface SpaceSidePanelProps {
  wsId: string;
  mode: SpacePanelMode;
}

export function SpaceSidePanel({ wsId, mode }: SpaceSidePanelProps) {
  return (
    <div className="h-full min-h-0">
      {mode === "files" && <SpaceFilesTab wsId={wsId} />}
      {mode === "tasks" && <SpaceTasksPanel wsId={wsId} />}
      {mode === "memory" && <SpaceMemoryTab wsId={wsId} />}
    </div>
  );
}
