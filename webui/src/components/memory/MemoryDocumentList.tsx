// 记忆文档列表视图 —— 取数 + 搜索 / 来源筛选 + 表格 + 翻页 + 详情抽屉 + 删除 + 空 / 错状态

import { useCallback, useEffect, useImperativeHandle, useState } from "react";
import axios from "axios";
import { Search, Trash2 } from "lucide-react";
import { toast } from "sonner";
import {
  deleteMemoryDocument,
  listMemoryDocuments,
  searchMemoryDocuments,
} from "@/api/memory";
import type {
  MemoryDocumentItem,
  MemoryDocumentPage,
  MemorySearchHit,
} from "@/types/memory";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
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
import { MemoryDeleteDialog } from "@/components/memory/MemoryDeleteDialog";
import { MemoryDocumentDetailSheet } from "@/components/memory/MemoryDocumentDetailSheet";

/** 供父组件（`MemoryPage`）触发重新拉取的句柄。 */
export interface MemoryDocumentListHandle {
  refresh: () => void;
}

interface MemoryDocumentListProps {
  ref?: React.Ref<MemoryDocumentListHandle>;
}

/** 固定页大小（不做 page size 选择器）。 */
const PAGE_LIMIT = 20;

/** 检索返回条数上限（后端 clamp 1..=50，前端默认 20）。 */
const SEARCH_LIMIT = 20;

/** 画像记忆受保护 —— 置灰而非隐藏：隐藏会让用户以为 UI 缺功能，置灰给出确定原因。 */
const PROFILE_SOURCE = "ppa_profile";
const PROFILE_DELETE_TITLE = "画像记忆（ppa_profile）受保护，不支持删除";

/** 来源下拉「全部」的哨兵值：radix `Select` 把空串 `""` 视作未选中/占位，故不用空串；对外仍映射为「不带 `source` 参数」。 */
const SOURCE_ALL = "__all__";
const SOURCE_OPTIONS = ["ppa_episodic", "ppa_semantic", "ppa_profile"];

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
  // 列表态
  const [offset, setOffset] = useState(0);
  const [page, setPage] = useState<MemoryDocumentPage | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // 无该库时后端 404，视为空列表。
  const [notFound, setNotFound] = useState(false);

  // 条件栏（列表态与检索态共用）
  const [source, setSource] = useState("");
  const [input, setInput] = useState("");
  // 已提交的检索词（非空 ⇒ 检索态）
  const [query, setQuery] = useState("");

  // 检索态
  const [hits, setHits] = useState<MemorySearchHit[]>([]);
  const [searchLoading, setSearchLoading] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);

  // 详情抽屉
  const [detailId, setDetailId] = useState<string | null>(null);

  // 删除确认
  const [deleteTarget, setDeleteTarget] = useState<MemoryDocumentItem | null>(
    null,
  );
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  const searching = query !== "";

  const loadList = useCallback(async () => {
    setLoading(true);
    setError(null);
    setNotFound(false);
    try {
      setPage(
        source
          ? await listMemoryDocuments(offset, PAGE_LIMIT, source)
          : await listMemoryDocuments(offset, PAGE_LIMIT),
      );
    } catch (err) {
      if (axios.isAxiosError(err) && err.response?.status === 404) {
        setNotFound(true);
      } else {
        setError(getApiErrorMessage(err) ?? "加载记忆文档失败");
      }
    } finally {
      setLoading(false);
    }
  }, [offset, source]);

  /**
   * 删后刷新**当前页**，页被删空则回退一页 —— 留在空页时用户只会看到「本页无数据」，
   * 误以为删除把整页都清掉了。
   */
  const refreshAfterDelete = useCallback(async () => {
    setLoading(true);
    setError(null);
    setNotFound(false);
    try {
      const next = source
        ? await listMemoryDocuments(offset, PAGE_LIMIT, source)
        : await listMemoryDocuments(offset, PAGE_LIMIT);
      setPage(next);
      if (next.documents.length === 0 && offset > 0) {
        setOffset(Math.max(0, offset - PAGE_LIMIT));
      }
    } catch (err) {
      if (axios.isAxiosError(err) && err.response?.status === 404) {
        setNotFound(true);
      } else {
        setError(getApiErrorMessage(err) ?? "加载记忆文档失败");
      }
    } finally {
      setLoading(false);
    }
  }, [offset, source]);

  const runSearch = useCallback(async () => {
    if (query === "") return;
    setSearchLoading(true);
    setSearchError(null);
    try {
      const res = source
        ? await searchMemoryDocuments(query, SEARCH_LIMIT, source)
        : await searchMemoryDocuments(query, SEARCH_LIMIT);
      setHits(res.hits);
    } catch (err) {
      setSearchError(getApiErrorMessage(err) ?? "检索记忆文档失败");
    } finally {
      setSearchLoading(false);
    }
  }, [query, source]);

  // 列表态取数（检索态下不取列表）。
  useEffect(() => {
    if (!searching) void loadList();
  }, [loadList, searching]);

  // 检索态取数（含 source 变更后重跑）。
  useEffect(() => {
    if (searching) void runSearch();
  }, [runSearch, searching]);

  useImperativeHandle(
    ref,
    () => ({
      // 检索态下刷新 = 重跑当前检索（不回列表），避免「输入框有 q、表格是列表」的不一致态。
      refresh: () => {
        if (searching) void runSearch();
        else void loadList();
      },
    }),
    [searching, runSearch, loadList],
  );

  const submitSearch = () => {
    setOffset(0);
    setQuery(input.trim());
  };

  const clearFilters = () => {
    setInput("");
    setQuery("");
    setSource("");
    setOffset(0);
    setHits([]);
  };

  const openDelete = (doc: MemoryDocumentItem) => {
    setDeleteError(null);
    setDeleteTarget(doc);
  };

  const closeDelete = () => {
    setDeleteTarget(null);
    setDeleteError(null);
  };

  const confirmDelete = async () => {
    if (!deleteTarget) return;
    const docId = deleteTarget.id;
    setDeleting(true);
    setDeleteError(null);
    try {
      await deleteMemoryDocument(docId);
      toast.success("已删除");
      closeDelete();
      await refreshAfterDelete();
    } catch (err) {
      const status = axios.isAxiosError(err) ? err.response?.status : undefined;
      if (status === 403) {
        // UI 状态已过期（该行本就受保护），重拉把按钮置灰，不留一个点了才报错的入口。
        toast.error(getApiErrorMessage(err) ?? "该文档不支持删除");
        closeDelete();
        void loadList();
      } else if (status === 404) {
        toast.warning("该文档已被删除");
        closeDelete();
        await refreshAfterDelete();
      } else {
        // 网络 / 409 / 500：留在弹层内，用户可原地重试或取消。
        setDeleteError(getApiErrorMessage(err) ?? "删除失败");
      }
    } finally {
      setDeleting(false);
    }
  };

  const onSourceChange = (value: string) => {
    setSource(value === SOURCE_ALL ? "" : value);
    setOffset(0);
  };

  // ── 条件栏 ───────────────────────────────────────────────────────────────
  const conditionBar = (
    <div className="flex flex-wrap items-center gap-2">
      <div className="relative min-w-[200px] flex-1">
        <Search className="absolute top-1/2 left-3 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
        <Input
          className="h-9 pl-9"
          placeholder="搜索记忆内容…"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submitSearch();
          }}
        />
      </div>
      <Select
        value={source === "" ? SOURCE_ALL : source}
        onValueChange={onSourceChange}
      >
        <SelectTrigger className="w-[160px]" aria-label="来源筛选">
          <SelectValue placeholder="全部来源" />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={SOURCE_ALL}>全部</SelectItem>
          {SOURCE_OPTIONS.map((s) => (
            <SelectItem key={s} value={s}>
              {s}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <Button variant="outline" size="sm" onClick={submitSearch}>
        搜索
      </Button>
      <Button variant="ghost" size="sm" onClick={clearFilters}>
        清除
      </Button>
    </div>
  );

  // ── 主体 ─────────────────────────────────────────────────────────────────
  let body: React.ReactNode;

  if (searching) {
    if (searchLoading) {
      body = (
        <div className="space-y-2" data-testid="memory-search-loading">
          {Array.from({ length: 5 }).map((_, i) => (
            <Skeleton key={i} className="h-10 w-full" />
          ))}
        </div>
      );
    } else if (searchError) {
      body = (
        <ErrorBanner message={searchError} onRetry={() => void runSearch()} />
      );
    } else if (hits.length === 0) {
      body = (
        <EmptyState
          title="没有匹配的记忆文档"
          description={`未在正文中找到「${query}」`}
          action={
            <Button variant="outline" size="sm" onClick={clearFilters}>
              清除筛选
            </Button>
          }
        />
      );
    } else {
      body = (
        <div className="space-y-2">
          <p className="text-xs text-muted-foreground">命中 {hits.length} 条</p>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>标题</TableHead>
                <TableHead>来源</TableHead>
                <TableHead>类型</TableHead>
                <TableHead>命中片段</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {hits.map((hit) => (
                <TableRow
                  key={hit.id}
                  role="button"
                  tabIndex={0}
                  className="cursor-pointer"
                  onClick={() => setDetailId(hit.id)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      setDetailId(hit.id);
                    }
                  }}
                >
                  <TableCell
                    className="max-w-[220px] truncate"
                    title={hit.title}
                  >
                    {hit.title}
                  </TableCell>
                  <TableCell>
                    <Badge variant="secondary">{hit.source}</Badge>
                  </TableCell>
                  <TableCell>{hit.file_type ?? "-"}</TableCell>
                  <TableCell
                    className="max-w-[360px] truncate"
                    title={hit.snippet}
                  >
                    {hit.snippet}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      );
    }
  } else if (loading) {
    body = (
      <div className="space-y-2" data-testid="memory-documents-loading">
        {Array.from({ length: 5 }).map((_, i) => (
          <Skeleton key={i} className="h-10 w-full" />
        ))}
      </div>
    );
  } else if (notFound) {
    body = (
      <EmptyState
        title="还没有记忆"
        description="开始对话后，Peco 会在这里沉淀你的长期记忆"
      />
    );
  } else if (error) {
    body = <ErrorBanner message={error} onRetry={() => void loadList()} />;
  } else {
    const documents = page?.documents ?? [];
    if (documents.length === 0 && offset > 0) {
      body = (
        <p className="py-10 text-center text-sm text-muted-foreground">
          本页无数据，返回上一页
        </p>
      );
    } else if (documents.length === 0 && source) {
      body = (
        <EmptyState
          title="该来源下没有记忆文档"
          description={`来源：${source}`}
          action={
            <Button variant="outline" size="sm" onClick={clearFilters}>
              清除筛选
            </Button>
          }
        />
      );
    } else if (documents.length === 0) {
      body = (
        <EmptyState
          title="还没有记忆文档"
          description="与 peco 对话后会自动沉淀记忆文档"
        />
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
              <TableHead className="w-12" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {documents.map((doc) => {
              const isProfile = doc.source === PROFILE_SOURCE;
              return (
                <TableRow
                  key={doc.id}
                  role="button"
                  tabIndex={0}
                  className="cursor-pointer"
                  onClick={() => setDetailId(doc.id)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      setDetailId(doc.id);
                    }
                  }}
                >
                  <TableCell
                    className="max-w-[280px] truncate"
                    title={doc.title}
                  >
                    {doc.title}
                  </TableCell>
                  <TableCell>
                    <Badge variant="secondary">{doc.source}</Badge>
                  </TableCell>
                  <TableCell>{doc.file_type ?? "-"}</TableCell>
                  <TableCell className="font-mono text-xs" title={doc.id}>
                    {doc.id}
                  </TableCell>
                  <TableCell>
                    <Button
                      variant="ghost"
                      size="icon"
                      aria-label="删除记忆"
                      aria-disabled={isProfile}
                      disabled={isProfile}
                      title={isProfile ? PROFILE_DELETE_TITLE : "删除记忆"}
                      data-testid={`memory-doc-delete-${doc.id}`}
                      onClick={(e) => {
                        // 行本身点击进详情，行内按钮必须自己截断冒泡。
                        e.stopPropagation();
                        openDelete(doc);
                      }}
                      onKeyDown={(e) => {
                        e.stopPropagation();
                      }}
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      );
    }
  }

  // 分页条仅列表态显示（E4 无分页）。
  const step = page?.limit || PAGE_LIMIT;
  const hasMore = page?.has_more ?? false;
  const pagination = !searching && !loading && !notFound && !error && (
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
  );

  return (
    <div className="space-y-4">
      {conditionBar}
      {body}
      {pagination}
      <MemoryDocumentDetailSheet
        docId={detailId}
        onClose={() => setDetailId(null)}
        onRefreshList={() => void loadList()}
      />
      <MemoryDeleteDialog
        open={deleteTarget !== null}
        title="确认删除这条记忆？"
        description={
          <>
            <span className="block">
              将永久删除记忆文档「
              <span className="font-semibold">{deleteTarget?.title}</span>
              」。它的向量索引、全文索引与图谱边会一并清理，删除后不会再被检索或召回。
            </span>
            <span className="block">
              本次删除会写入删除审计（保留原文），但界面内不提供一键恢复。
            </span>
          </>
        }
        busy={deleting}
        error={deleteError}
        onCancel={closeDelete}
        onConfirm={() => void confirmDelete()}
        testId="memory-doc-delete-dialog"
      />
    </div>
  );
}
