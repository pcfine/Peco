// 知识库 tab：名字 + 文档数。清单来自 `/api/knowledge`（团队命名空间），
// `document_count` 也用接口给的那份，不在前端数文档列表。

import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { useKnowledgeBases } from "@/queries/teamDetail";
import { errorText } from "@/lib/api";
import type { TeamDetail } from "@/types/daemon";

export function TeamKnowledgeTab({
  wsId,
  detail,
}: {
  wsId: string;
  detail: TeamDetail;
}) {
  const bases = useKnowledgeBases(wsId);

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
  if (rows.length === 0) {
    // 索引表与目录可能各自为空：团队详情已经告诉我们「装了哪些」，两处都说没有
    // 才是真的没有，免得把「接口空」当成「没有知识库」。
    return (
      <p className="max-w-[480px] p-3 text-sm text-ink-muted">
        这个团队还没有知识库
        {detail.knowledge_bases.length > 0
          ? `（团队目录里有 ${detail.knowledge_bases.map((kb) => kb.name).join("、")}，索引还没同步）`
          : "。"}
      </p>
    );
  }

  return (
    <ul>
      {rows.map((kb) => (
        <li
          key={kb.id}
          className="flex h-9 items-center gap-2 border-b border-line px-3"
        >
          <span className="min-w-0 flex-1 truncate text-sm text-ink">
            {kb.name}
          </span>
          <span className="shrink-0 tabular-nums text-caption font-medium text-ink-faint">
            文档 {kb.document_count}
          </span>
          <span className="w-28 shrink-0 truncate text-right font-mono text-2xs text-ink-faint">
            {kb.embedding_model}
          </span>
        </li>
      ))}
    </ul>
  );
}
