// 知识库 tab：卡片列表（名字 + 描述 + 规模）+ 表单化新建。清单来自 `/api/knowledge`
// （团队命名空间），`document_count` 也用接口给的那份，不在前端数文档列表。

import { useState } from "react";
import { CardGrid, EntityCard } from "@/components/common/CardGrid";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { NewKnowledgeDialog } from "@/components/teams/NewKnowledgeDialog";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { useKnowledgeBases } from "@/queries/teamDetail";
import { errorText, type ResourceScope } from "@/lib/api";
import type { KnowledgeBase, TeamDetail } from "@/types/daemon";

function KnowledgeCard({ kb }: { kb: KnowledgeBase }) {
  return (
    <EntityCard>
      <div className="flex min-w-0 flex-col gap-1">
        <span className="truncate text-sm font-semibold text-ink">
          {kb.name}
        </span>
        <p className="line-clamp-2 text-caption text-ink-muted">
          {kb.description || "无描述"}
        </p>
        <p className="mt-auto truncate font-mono text-caption tabular-nums text-ink-faint">
          {kb.document_count} 文档 · {kb.chunk_count} 分块 ·{" "}
          {kb.embedding_model}
        </p>
      </div>
    </EntityCard>
  );
}

export function TeamKnowledgeTab({
  scope,
  detail,
}: {
  scope: ResourceScope;
  detail: TeamDetail;
}) {
  const bases = useKnowledgeBases(scope);
  const [creating, setCreating] = useState(false);

  if (bases.error) {
    return (
      <div className="p-3">
        <ErrorBanner
          message={errorText(bases.error)}
          onRetry={() => void bases.refetch()}
        />
      </div>
    );
  }
  if (bases.isPending) {
    return (
      <div className="space-y-px p-3">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-9 w-full" />
        ))}
      </div>
    );
  }

  const rows = bases.data ?? [];
  // 索引表与目录可能各自为空：团队详情已经告诉我们「装了哪些」，两处都说没有
  // 才是真的没有，免得把「接口空」当成「没有知识库」。
  const emptyText =
    detail.knowledge_bases.length > 0
      ? `这个团队还没有知识库（团队目录里有 ${detail.knowledge_bases.map((kb) => kb.name).join("、")}，索引还没同步）`
      : "这个团队还没有知识库";

  return (
    <div className="p-3">
      <div className="mb-3 flex items-center gap-2">
        <Button size="sm" onClick={() => setCreating(true)}>
          ＋ 新建知识库
        </Button>
        <span className="ml-auto flex items-center gap-2">
          <span className="text-sm font-medium text-ink">知识库</span>
          <span className="text-caption text-ink-faint">{rows.length}</span>
        </span>
      </div>
      {rows.length === 0 ? (
        <EmptyState title={emptyText} />
      ) : (
        <CardGrid>
          {rows.map((kb) => (
            <KnowledgeCard key={kb.id} kb={kb} />
          ))}
        </CardGrid>
      )}
      <NewKnowledgeDialog
        scope={scope}
        open={creating}
        onOpenChange={setCreating}
      />
    </div>
  );
}
