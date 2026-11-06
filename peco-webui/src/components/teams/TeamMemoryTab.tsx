// 记忆 tab：本期只有占位。daemon 还没有记忆读写接口，这里不摆假数据 ——
// 空态写明「尚未接入」，比一张永远不变的表格诚实。

import { EmptyState } from "@/components/common/EmptyState";

export function TeamMemoryTab() {
  return (
    <div className="p-3">
      <EmptyState title="记忆接口尚未接入。团队的记忆内容与召回记录都要等 daemon 提供读写接口后才能在这里展示。" />
    </div>
  );
}
