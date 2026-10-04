// 记忆「历史」tab 列表 —— 被取代条目 + 后继 / 保留期 / 状态 + 回滚入口
// （design §11 展示分层；裸数组分页，无 has_more 信封）

import { useCallback, useEffect, useImperativeHandle, useState } from "react";
import axios from "axios";
import { toast } from "sonner";
import { listMemoryAudit, restoreMemoryAudit } from "@/api/memory";
import type { MemoryAuditItem } from "@/types/memory";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
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
export interface MemoryAuditListHandle {
  refresh: () => void;
}

interface MemoryAuditListProps {
  ref?: React.Ref<MemoryAuditListHandle>;
}

/** 固定页大小（后端默认 20 / 上限 100）。 */
const PAGE_LIMIT = 20;

/** 历史 tab 恒按被取代原因过滤（后端缺省 / 空串不过滤）。 */
const REASON_SUPERSEDED = "superseded";

/** 后端 `ApiError` → 可读文案（与 `MemoryDocumentList` 同口径）。 */
function getApiErrorMessage(err: unknown): string | undefined {
  if (axios.isAxiosError(err)) {
    if (err.response?.data?.details) return String(err.response.data.details);
    if (err.response?.data?.message) return String(err.response.data.message);
    if (err.message) return err.message;
  }
  if (err instanceof Error) return err.message;
  return undefined;
}

/** `deleted_at` 本地化展示；解析失败回退原串（不冒充合法时间）。 */
function formatDeletedAt(raw: string): string {
  const at = new Date(raw);
  return Number.isNaN(at.getTime()) ? raw : at.toLocaleString("zh-CN");
}

/** 状态徽章色：pending 走主色引注意，done 次要色，cancelled 弱化。 */
function statusVariant(
  status: string,
): "default" | "secondary" | "outline" | "destructive" {
  if (status === "pending") return "default";
  if (status === "done") return "secondary";
  if (status === "cancelled") return "outline";
  return "outline";
}

export function MemoryAuditList({ ref }: MemoryAuditListProps) {
  // 列表态（追加式分页：offset 仅记录已加载到的位置）
  const [items, setItems] = useState<MemoryAuditItem[]>([]);
  const [offset, setOffset] = useState(0);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [hasMore, setHasMore] = useState(false);

  // 回滚确认
  const [restoreTarget, setRestoreTarget] = useState<MemoryAuditItem | null>(
    null,
  );
  const [restoring, setRestoring] = useState(false);
  const [restoreError, setRestoreError] = useState<string | null>(null);

  const loadPage = useCallback(async (at: number) => {
    const initial = at === 0;
    if (initial) {
      setLoading(true);
      setError(null);
    } else {
      setLoadingMore(true);
    }
    try {
      const rows = await listMemoryAudit(at, PAGE_LIMIT, REASON_SUPERSEDED);
      setItems((prev) => (initial ? rows : [...prev, ...rows]));
      setOffset(at);
      // 裸数组无信封 —— 返回条数 < limit 视为到底
      setHasMore(rows.length >= PAGE_LIMIT);
    } catch (err) {
      if (initial) setError(getApiErrorMessage(err) ?? "加载历史记录失败");
      else toast.error(getApiErrorMessage(err) ?? "加载更多失败");
    } finally {
      if (initial) setLoading(false);
      else setLoadingMore(false);
    }
  }, []);

  // 首屏取数（refresh 走 handle，重置回第一页）。
  useEffect(() => {
    void loadPage(0);
  }, [loadPage]);

  useImperativeHandle(
    ref,
    () => ({
      refresh: () => void loadPage(0),
    }),
    [loadPage],
  );

  const openRestore = (item: MemoryAuditItem) => {
    setRestoreError(null);
    setRestoreTarget(item);
  };

  const closeRestore = () => {
    setRestoreTarget(null);
    setRestoreError(null);
  };

  const confirmRestore = async () => {
    if (!restoreTarget) return;
    setRestoring(true);
    setRestoreError(null);
    try {
      await restoreMemoryAudit(restoreTarget.id);
      toast.success("已回滚");
      setRestoreTarget(null);
      await loadPage(0);
    } catch (err) {
      // 4xx（跨槽 409 / 校验）与 5xx（retire 失败）都如实展示，弹层保留供重试
      setRestoreError(getApiErrorMessage(err) ?? "回滚失败");
    } finally {
      setRestoring(false);
    }
  };

  // ── 主体 ─────────────────────────────────────────────────────────────────
  let body: React.ReactNode;

  if (loading) {
    body = (
      <div className="space-y-2" data-testid="memory-audit-loading">
        {Array.from({ length: 5 }).map((_, i) => (
          <Skeleton key={i} className="h-10 w-full" />
        ))}
      </div>
    );
  } else if (error) {
    body = <ErrorBanner message={error} onRetry={() => void loadPage(0)} />;
  } else if (items.length === 0) {
    body = (
      <EmptyState
        title="暂无被取代的记忆条目"
        description="取代执行开启后，此处显示被取代条目、其后继与剩余保留天数"
      />
    );
  } else {
    body = (
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>标题</TableHead>
            <TableHead>变更时间</TableHead>
            <TableHead>后继</TableHead>
            <TableHead>保留期</TableHead>
            <TableHead>状态</TableHead>
            <TableHead>操作</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {items.map((item) => {
            const restored = Boolean(item.restored_at);
            return (
              <TableRow key={item.id}>
                <TableCell
                  className="max-w-[240px] truncate"
                  title={item.title}
                >
                  {item.title}
                </TableCell>
                <TableCell className="whitespace-nowrap">
                  {formatDeletedAt(item.deleted_at)}
                </TableCell>
                <TableCell
                  className="max-w-[200px] truncate"
                  title={item.successor_title}
                >
                  {item.successor_title || "—"}
                </TableCell>
                <TableCell className="whitespace-nowrap">
                  {item.retention_days_remaining === undefined
                    ? "—"
                    : `剩余 ${item.retention_days_remaining} 天`}
                </TableCell>
                <TableCell>
                  <Badge variant={statusVariant(item.status)}>
                    {item.status}
                  </Badge>
                </TableCell>
                <TableCell>
                  <div className="flex items-center gap-2">
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={restored}
                      onClick={() => openRestore(item)}
                    >
                      回滚
                    </Button>
                    {restored && <Badge variant="outline">已回滚</Badge>}
                  </div>
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
    );
  }

  return (
    <div className="space-y-4">
      {body}
      {hasMore && !loading && !error && (
        <div className="flex justify-center">
          <Button
            variant="outline"
            size="sm"
            disabled={loadingMore}
            onClick={() => void loadPage(offset + PAGE_LIMIT)}
          >
            {loadingMore ? "加载中…" : "加载更多"}
          </Button>
        </div>
      )}

      <Dialog
        open={!!restoreTarget}
        onOpenChange={(open) => {
          if (!open) closeRestore();
        }}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>确认回滚</DialogTitle>
            <DialogDescription>
              确定回滚对「
              <span className="font-semibold">{restoreTarget?.title}</span>」的
              删除吗？条目将被重新写回知识库，当前存活的后继将被退役。
            </DialogDescription>
          </DialogHeader>
          {restoreError && (
            <p className="text-sm text-destructive">{restoreError}</p>
          )}
          <DialogFooter>
            <Button
              variant="outline"
              disabled={restoring}
              onClick={closeRestore}
            >
              取消
            </Button>
            <Button disabled={restoring} onClick={() => void confirmRestore()}>
              {restoring ? "回滚中…" : "回滚"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
