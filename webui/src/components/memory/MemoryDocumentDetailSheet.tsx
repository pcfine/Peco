// 记忆文档详情右侧抽屉 —— 打开即请求，自带 loading / 成功 / 空正文 / 404 / 错误态

import { useCallback, useEffect, useState } from "react";
import axios from "axios";
import { getMemoryDocument } from "@/api/memory";
import type { MemoryDocumentDetail } from "@/types/memory";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { ErrorBanner } from "@/components/common/ErrorBanner";

interface MemoryDocumentDetailSheetProps {
  /** 受控：`null` ⇒ 关闭；非空 ⇒ 打开并请求该 id。 */
  docId: string | null;
  /** 关闭（Esc / 遮罩 / X 均经 radix `onOpenChange(false)` 汇聚到此）。 */
  onClose: () => void;
  /** 404 态「刷新列表」按钮（可选，由父组件传入；不自动重取）。 */
  onRefreshList?: () => void;
}

/** 后端 `ApiError` → 可读文案（与列表视图同口径的局部 helper）。 */
function getApiErrorMessage(err: unknown): string | undefined {
  if (axios.isAxiosError(err)) {
    if (err.response?.data?.details) return String(err.response.data.details);
    if (err.response?.data?.message) return String(err.response.data.message);
    if (err.message) return err.message;
  }
  if (err instanceof Error) return err.message;
  return undefined;
}

export function MemoryDocumentDetailSheet({
  docId,
  onClose,
  onRefreshList,
}: MemoryDocumentDetailSheetProps) {
  const [data, setData] = useState<MemoryDocumentDetail | null>(null);
  const [loading, setLoading] = useState(false);
  const [notFound, setNotFound] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async (id: string) => {
    setLoading(true);
    setNotFound(false);
    setError(null);
    try {
      setData(await getMemoryDocument(id));
    } catch (err) {
      // 404 与其它错误分流：前者是「文档不存在」，后者可重试。
      if (axios.isAxiosError(err) && err.response?.status === 404) {
        setData(null);
        setNotFound(true);
      } else {
        setError(getApiErrorMessage(err) ?? "加载文档详情失败");
      }
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (docId === null) {
      // 关闭时清空内部状态 —— **不触碰**父组件的列表 page / offset / loading。
      setData(null);
      setNotFound(false);
      setError(null);
      return;
    }
    void load(docId);
  }, [docId, load]);

  const open = docId !== null;

  return (
    <Sheet
      open={open}
      onOpenChange={(next) => {
        if (!next) onClose();
      }}
    >
      <SheetContent
        side="right"
        className="flex w-full flex-col gap-0 p-0 sm:max-w-lg"
        data-testid="memory-document-detail"
      >
        <SheetHeader className="border-b">
          <SheetTitle className="pr-8 text-left break-words">
            {loading ? (
              <Skeleton className="h-5 w-40" />
            ) : data ? (
              data.title || "（无标题）"
            ) : (
              "文档详情"
            )}
          </SheetTitle>
          {!loading && data && (
            <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
              <Badge variant="secondary">{data.source}</Badge>
              <span>类型 {data.file_type ?? "—"}</span>
              {data.created_at && (
                <span data-testid="memory-detail-created-at">
                  {new Date(data.created_at).toLocaleString()}
                </span>
              )}
              <span>约 {data.content.length} 字</span>
            </div>
          )}
        </SheetHeader>

        {loading && (
          <div className="space-y-2 p-4" data-testid="memory-detail-loading">
            {Array.from({ length: 6 }).map((_, i) => (
              <Skeleton key={i} className="h-4 w-full" />
            ))}
          </div>
        )}

        {!loading && notFound && (
          <div className="flex flex-col items-start gap-3 p-4">
            <p className="text-sm text-muted-foreground">
              文档不存在或已被删除
            </p>
            <div className="flex gap-2">
              <Button variant="outline" size="sm" onClick={onClose}>
                关闭
              </Button>
              {onRefreshList && (
                <Button variant="outline" size="sm" onClick={onRefreshList}>
                  刷新列表
                </Button>
              )}
            </div>
          </div>
        )}

        {!loading && !notFound && error && (
          <div className="p-4">
            <ErrorBanner
              message={error}
              onRetry={() => {
                if (docId !== null) void load(docId);
              }}
            />
          </div>
        )}

        {!loading && !notFound && !error && data && (
          <>
            {data.content === "" ? (
              <p className="p-4 text-sm text-muted-foreground">
                该文档没有正文内容
              </p>
            ) : (
              <ScrollArea className="min-h-0 flex-1">
                <div className="p-4 text-sm whitespace-pre-wrap break-words">
                  {data.content}
                </div>
              </ScrollArea>
            )}
          </>
        )}
      </SheetContent>
    </Sheet>
  );
}
