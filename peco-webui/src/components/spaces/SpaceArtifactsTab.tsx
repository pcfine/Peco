// 产物：清单来自 `/artifacts`，预览走 `/files` —— 两者的 `rel_path` 同源于空间
// work 目录，因此文本预览不需要第二条取流通道；二进制直接给下载链接。

import { useState } from "react";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { MarkdownRenderer } from "@/components/chat/MarkdownRenderer";
import { useSpaceFiles } from "@/queries/spaceDetail";
import { errorText } from "@/lib/api";
import { formatBytes, formatCreatedAt } from "@/lib/format";
import { relativeTime } from "@/lib/relative";
import type { SpaceArtifact } from "@/types/daemon";

interface SpaceArtifactsTabProps {
  wsId: string;
  artifacts: SpaceArtifact[];
  isPending: boolean;
  error: unknown;
  onRetry: () => void;
}

function ArtifactPreview({
  wsId,
  artifact,
  onClose,
}: {
  wsId: string;
  artifact: SpaceArtifact | null;
  onClose: () => void;
}) {
  const files = useSpaceFiles(wsId, artifact?.rel_path ?? "", {
    enabled: artifact !== null,
  });
  const body = files.data && "kind" in files.data ? files.data : null;

  return (
    <Dialog
      open={artifact !== null}
      onOpenChange={(next) => !next && onClose()}
    >
      <DialogContent className="h-[min(80vh,720px)] w-[min(90vw,960px)] max-w-[min(90vw,960px)]! gap-2">
        <DialogHeader>
          <DialogTitle className="truncate font-mono text-sm">
            {artifact?.rel_path}
          </DialogTitle>
        </DialogHeader>
        <div className="min-h-0 flex-1 overflow-y-auto">
          {files.error && <ErrorBanner message={errorText(files.error)} />}
          {files.isPending && <Skeleton className="h-16 w-full" />}
          {body?.binary && (
            <p className="max-w-narrow text-sm text-ink-muted">
              这是二进制文件，不在这里渲染。
            </p>
          )}
          {body && !body.binary && body.text !== undefined && (
            <MarkdownRenderer content={body.text} />
          )}
        </div>
        {artifact && (
          <div className="flex shrink-0 items-center gap-3 border-t border-line pt-2 text-caption font-medium text-ink-faint">
            <a
              href={`/api/artifacts/${encodeURIComponent(artifact.id)}/download`}
              className="text-primary-ink hover:underline"
            >
              下载 {formatBytes(artifact.size_bytes)}
            </a>
            <span>{artifact.kind}</span>
            <span>{formatCreatedAt(artifact.created_at)}</span>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}

export function SpaceArtifactsTab({
  wsId,
  artifacts,
  isPending,
  error,
  onRetry,
}: SpaceArtifactsTabProps) {
  const [preview, setPreview] = useState<SpaceArtifact | null>(null);

  if (error) {
    return (
      <div className="p-3">
        <ErrorBanner message={errorText(error)} onRetry={onRetry} />
      </div>
    );
  }

  if (isPending) {
    return (
      <div className="space-y-px p-3">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-9 w-full" />
        ))}
      </div>
    );
  }

  if (artifacts.length === 0) {
    return (
      <div className="p-3">
        <EmptyState title="任务完成后，产物会出现在这里" />
      </div>
    );
  }

  return (
    <div className="h-full overflow-y-auto">
      <div className="sticky top-0 z-10 flex h-8 items-center gap-2 border-b border-line bg-panel px-3 text-caption font-medium text-ink-faint">
        <span className="w-16 shrink-0">类型</span>
        <span className="min-w-0 flex-1">名称</span>
        <span className="w-20 shrink-0 text-right">大小</span>
        <span className="w-24 shrink-0 text-right">时间</span>
      </div>
      <ul>
        {artifacts.map((artifact) => (
          <li key={artifact.id} className="border-b border-line">
            <button
              type="button"
              onClick={() => setPreview(artifact)}
              className="flex h-9 w-full items-center gap-2 px-3 text-left hover:bg-line/30"
            >
              <span className="w-16 shrink-0 truncate font-mono text-2xs uppercase text-ink-faint">
                {artifact.kind}
              </span>
              <span className="min-w-0 flex-1 truncate text-sm text-ink">
                {artifact.rel_path}
              </span>
              <span className="w-20 shrink-0 text-right tabular-nums text-2xs text-ink-faint">
                {formatBytes(artifact.size_bytes)}
              </span>
              <span className="w-24 shrink-0 text-right tabular-nums text-caption font-medium text-ink-faint">
                {relativeTime(artifact.created_at)}
              </span>
            </button>
          </li>
        ))}
      </ul>
      <ArtifactPreview
        wsId={wsId}
        artifact={preview}
        onClose={() => setPreview(null)}
      />
    </div>
  );
}
