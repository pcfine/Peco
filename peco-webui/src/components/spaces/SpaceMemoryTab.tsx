// 右栏的记忆模式：本轮只有空态。
//
// 组件边界就是 `wsId`（记忆按空间过滤），接后端时只在这里加数据源，
// 调用方不需要跟着改。

import { EmptyState } from "@/components/common/EmptyState";

export function SpaceMemoryTab({ wsId }: { wsId: string }) {
  return (
    <div className="p-3" data-ws-id={wsId}>
      <EmptyState title="本空间还没有记忆。" />
      <p className="mt-1 max-w-[480px] text-2xs text-ink-faint">
        对话里确认下来的偏好与事实会记在这个空间名下；记忆浏览接上后出现在这里。
      </p>
    </div>
  );
}
