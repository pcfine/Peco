// 记忆文档列表视图 —— 取数 + 表格 + 翻页 + 空 / 错状态（design §3.5.4 / §3.5.5）

import { useCallback, useEffect, useImperativeHandle, useState } from "react";
import axios from "axios";
import { listMemoryDocuments } from "@/api/memory";
import type { MemoryDocumentPage } from "@/types/memory";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";

/** 供父组件（`MemoryPage`）触发重新拉取的句柄。 */
export interface MemoryDocumentListHandle {
  refresh: () => void;
}

interface MemoryDocumentListProps {
  ref?: React.Ref<MemoryDocumentListHandle>;
}

/** 固定页大小（不做 page size 选择器）。 */
const PAGE_LIMIT = 20;

/** 后端 `ApiError` → 可读文案（与既有页面的局部 helper 同口径）。 */
function getApiErrorMessage(err: unknown): string | undefined {
  if (axios.isAxiosError(err)) {
    if (err.response?.data?.details) return String(err.response.data.details);
    if (err.response?.data?.message) return String(err.response.data.message);
    if (err.message) return err.message;
  }
  if (err instanceof Error) return err.message;
  return undefined;
}

export function MemoryDocumentList({ ref }: MemoryDocumentListProps) {
  const [offset, setOffset] = useState(0);
  const [page, setPage] = useState<MemoryDocumentPage | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setPage(await listMemoryDocuments(offset, PAGE_LIMIT));
    } catch (err) {
      setError(getApiErrorMessage(err) ?? "加载记忆文档失败");
    } finally {
      setLoading(false);
    }
  }, [offset]);

  useEffect(() => {
    void load();
  }, [load]);

  useImperativeHandle(ref, () => ({ refresh: load }), [load]);

  if (loading) {
    return (
      <div className="space-y-2" data-testid="memory-documents-loading">
        {Array.from({ length: 5 }).map((_, i) => (
          <Skeleton key={i} className="h-10 w-full" />
        ))}
      </div>
    );
  }

  if (error) {
    return <ErrorBanner message={error} onRetry={() => void load()} />;
  }

  const documents = page?.documents ?? [];
  const hasMore = page?.has_more ?? false;
  // 步进 = 本页响应回显的 limit（生产恒等于 PAGE_LIMIT）
  const step = page?.limit || PAGE_LIMIT;

  let body: React.ReactNode;
  if (documents.length === 0 && offset === 0) {
    body = (
      <EmptyState
        title="还没有记忆文档"
        description="与 peco 对话后会自动沉淀记忆文档"
      />
    );
  } else if (documents.length === 0) {
    body = (
      <p className="py-10 text-center text-sm text-muted-foreground">
        本页无数据，返回上一页
      </p>
    );
  } else {
    body = (
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>标题</TableHead>
            <TableHead>来源</TableHead>
            <TableHead>类型</TableHead>
            <TableHead>ID</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {documents.map((doc) => (
            <TableRow key={doc.id}>
              <TableCell className="max-w-[280px] truncate" title={doc.title}>
                {doc.title}
              </TableCell>
              <TableCell>
                <Badge variant="secondary">{doc.source}</Badge>
              </TableCell>
              <TableCell>{doc.file_type ?? "-"}</TableCell>
              <TableCell className="font-mono text-xs" title={doc.id}>
                {doc.id}
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    );
  }

  return (
    <div className="space-y-4">
      {body}
      <div className="flex items-center justify-between">
        <p className="text-xs text-muted-foreground">
          第 {Math.floor(offset / step) + 1} 页 · offset {offset}
        </p>
        <div className="flex gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={offset === 0}
            onClick={() => setOffset(Math.max(0, offset - step))}
          >
            上一页
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={!hasMore}
            onClick={() => setOffset(offset + step)}
          >
            下一页
          </Button>
        </div>
      </div>
    </div>
  );
}
