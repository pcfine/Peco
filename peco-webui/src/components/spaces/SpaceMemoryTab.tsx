// 右栏的「记忆」：只读浏览当前空间的记忆 —— 搜索、类别过滤、列表、详情。
//
// 组件边界就是 `wsId`（记忆按空间过滤）：空间页与 Peco 页把各自的 wsId 传进来，
// 面板内部不再判断「我在哪个空间」。删除、图谱、审计留在团队页。

import { useEffect, useState } from "react";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { useOpenSpace } from "@/components/spaces/useSpaceLifecycle";
import {
  useSpace,
  useSpaceMemoryDocument,
  useSpaceMemoryDocuments,
} from "@/queries/spaceDetail";
import { ApiError, errorText } from "@/lib/api";
import { formatCreatedAt } from "@/lib/format";
import { cn } from "@/lib/utils";
import type { Space } from "@/types/daemon";

/** 输入即查：连打时逐字打后端只会把列表刷成噪声，停顿 300ms 才把词交出去。 */
const SEARCH_DEBOUNCE_MS = 300;

const CATEGORY_LABELS: Record<string, string> = {
  profile: "偏好",
  semantic: "事实",
  episodic: "事项",
};

/** 过滤器的四档；`null` 是不加这一维条件，与「匹配到空分类」不是一回事。 */
const CATEGORY_OPTIONS: readonly { value: string | null; label: string }[] = [
  { value: null, label: "全部" },
  { value: "profile", label: CATEGORY_LABELS.profile },
  { value: "semantic", label: CATEGORY_LABELS.semantic },
  { value: "episodic", label: CATEGORY_LABELS.episodic },
];

/** 后端加了新分类也要显示得出来：认不出的就写标签本身，不吞行。 */
function categoryLabel(category: string): string {
  return CATEGORY_LABELS[category] ?? category;
}

/** 409 是「这个空间没有实例」，与真正的失败走两条路。 */
function isSpaceNotOpen(error: unknown): boolean {
  return error instanceof ApiError && error.code === "space_not_open";
}

function useDebouncedValue<T>(value: T, delayMs: number): T {
  const [settled, setSettled] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setSettled(value), delayMs);
    return () => clearTimeout(timer);
  }, [value, delayMs]);
  return settled;
}

function CategoryChip({ category }: { category: string }) {
  return (
    <span className="shrink-0 rounded-[4px] bg-line px-1 text-2xs text-ink-muted">
      {categoryLabel(category)}
    </span>
  );
}

/** 列表骨架：与最终行的两行高度对齐，取回数据时不跳。 */
function ListSkeleton() {
  return (
    <div className="space-y-px p-3">
      {[0, 1, 2, 3].map((row) => (
        <Skeleton key={row} className="h-9 w-full" />
      ))}
    </div>
  );
}

export function SpaceMemoryTab({ wsId }: { wsId: string }) {
  const [search, setSearch] = useState("");
  const [category, setCategory] = useState<string | null>(null);
  const [openId, setOpenId] = useState<string | null>(null);

  // 没打开的空间没有实例，清单请求只会换来 409：先问状态再决定要不要读，
  // 顺带让「打开空间」成功后清单自己就能接上（空间查询被失效 → 状态变 open）。
  const space = useSpace(wsId);
  const spaceOpen = space.data?.status === "open";

  const query = useDebouncedValue(search.trim(), SEARCH_DEBOUNCE_MS);
  const list = useSpaceMemoryDocuments(
    wsId,
    { query, category },
    { enabled: spaceOpen },
  );

  // 换了条件就收起详情：展开的行可能已经不在新列表里，正文会和列表讲两件事。
  useEffect(() => {
    setOpenId(null);
  }, [query, category]);

  // 缓存里的上一份清单在空间关掉之后还在：读不到就不摆出来，否则会和
  // 「空间还没打开」的提示互相打脸。
  const readable = spaceOpen && !list.error;
  const items = readable ? (list.data?.items ?? []) : [];
  const filtered = query !== "" || category !== null;
  const count = readable && list.data ? `${list.data.total} 条` : "";

  const clearFilters = () => {
    setSearch("");
    setCategory(null);
  };

  const listFailed = spaceOpen && list.error !== null;
  // 状态是 15s 前的快照，清单请求才是当下：两者冲突时以后者为准。
  const spaceClosedByServer = spaceOpen && isSpaceNotOpen(list.error);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="shrink-0 space-y-2 border-b border-line p-2">
        <Input
          type="search"
          value={search}
          onChange={(event) => setSearch(event.target.value)}
          placeholder="搜索记忆…"
          aria-label="搜索记忆"
          className="h-8 text-xs"
        />
        <div className="flex min-w-0 items-center gap-1">
          <div
            role="group"
            aria-label="按类别过滤"
            className="flex min-w-0 items-center gap-1 overflow-x-auto"
          >
            {CATEGORY_OPTIONS.map((option) => (
              <button
                key={option.label}
                type="button"
                aria-pressed={category === option.value}
                onClick={() => setCategory(option.value)}
                className={cn(
                  "h-7 shrink-0 rounded-[4px] px-2 text-2xs text-ink-muted hover:text-ink",
                  category === option.value && "bg-line text-ink",
                )}
              >
                {option.label}
              </button>
            ))}
          </div>
          <span
            role="status"
            data-testid="memory-count"
            className="ml-auto shrink-0 tabular-nums text-2xs text-ink-faint"
          >
            {count}
          </span>
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {space.isPending && <ListSkeleton />}

        {space.error && (
          <div className="p-3">
            <ErrorBanner
              message={errorText(space.error)}
              onRetry={() => void space.refetch()}
            />
          </div>
        )}

        {space.data && !spaceOpen && <SpaceClosedNotice space={space.data} />}

        {spaceOpen && list.isPending && <ListSkeleton />}

        {listFailed && !spaceClosedByServer && (
          <div className="p-3">
            <ErrorBanner
              message={errorText(list.error)}
              onRetry={() => void list.refetch()}
            />
          </div>
        )}

        {spaceClosedByServer && space.data && (
          <SpaceClosedNotice space={space.data} />
        )}

        {readable && list.data && items.length === 0 && (
          <div className="p-3">
            <EmptyState
              title={filtered ? "没有匹配的记忆。" : "本空间还没有记忆。"}
              action={
                filtered ? (
                  <Button variant="ghost" size="xs" onClick={clearFilters}>
                    清除筛选
                  </Button>
                ) : undefined
              }
            />
            {!filtered && (
              <p className="mt-1 max-w-[480px] text-2xs text-ink-faint">
                对话里确认下来的偏好与事实会记在这个空间名下。
              </p>
            )}
          </div>
        )}

        {items.length > 0 && (
          <ul>
            {items.map((doc) => {
              const open = openId === doc.id;
              const time = formatCreatedAt(doc.created_at);
              return (
                <li key={doc.id} className="border-b border-line">
                  <button
                    type="button"
                    aria-expanded={open}
                    onClick={() => setOpenId(open ? null : doc.id)}
                    className="flex w-full min-w-0 flex-col gap-0.5 px-3 py-1.5 text-left outline-none hover:bg-line/30 focus-visible:ring-[3px] focus-visible:ring-ring/35"
                  >
                    <span className="flex min-w-0 items-center gap-2">
                      <span className="min-w-0 flex-1 truncate text-sm text-ink">
                        {doc.title || "无标题"}
                      </span>
                      {time && (
                        <span className="shrink-0 tabular-nums text-2xs text-ink-faint">
                          {time}
                        </span>
                      )}
                    </span>
                    <span className="flex min-w-0 items-center gap-1.5 text-2xs text-ink-faint">
                      <CategoryChip category={doc.category} />
                      <span className="min-w-0 flex-1 truncate">
                        {doc.snippet}
                      </span>
                    </span>
                  </button>
                  {open && <MemoryDocumentBody wsId={wsId} docId={doc.id} />}
                </li>
              );
            })}
          </ul>
        )}

        {/* 只取第一页：说清「还有没列出来的」，别让总数和可见行数对不上。 */}
        {readable && list.data && list.data.total > items.length && (
          <p className="px-3 py-2 text-2xs text-ink-faint">
            已列出前 <span className="tabular-nums">{items.length}</span> 条，共{" "}
            <span className="tabular-nums">{list.data.total}</span>{" "}
            条；用搜索或类别过滤缩小范围。
          </p>
        )}
      </div>
    </div>
  );
}

/** 展开行的正文：列表只给摘要，全文要按 `docId` 单独取。 */
function MemoryDocumentBody({ wsId, docId }: { wsId: string; docId: string }) {
  const detail = useSpaceMemoryDocument(wsId, docId);

  if (detail.isPending) {
    return (
      <div className="space-y-1 border-t border-line bg-canvas/40 px-3 py-2">
        <Skeleton className="h-3 w-2/3" />
        <Skeleton className="h-12 w-full" />
      </div>
    );
  }

  if (detail.error) {
    return (
      <div className="border-t border-line bg-canvas/40 px-3 py-2">
        <ErrorBanner
          message={errorText(detail.error)}
          onRetry={() => void detail.refetch()}
        />
      </div>
    );
  }

  const doc = detail.data;
  if (!doc) return null;
  const time = formatCreatedAt(doc.created_at);

  return (
    <div className="space-y-1.5 border-t border-line bg-canvas/40 px-3 py-2">
      <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-2xs text-ink-faint">
        <CategoryChip category={doc.category} />
        {time && <span className="shrink-0 tabular-nums">{time}</span>}
        <span className="min-w-0 flex-1 truncate font-mono" title={doc.source}>
          来源 {doc.source}
        </span>
      </div>
      {doc.content ? (
        <p className="max-w-[640px] whitespace-pre-wrap break-words text-sm text-ink">
          {doc.content}
        </p>
      ) : (
        <p className="text-2xs text-ink-faint">这篇记忆没有正文。</p>
      )}
    </div>
  );
}

/** 未打开的空间没有记忆可读，所以要就地给一条打开的路，而不是让人去别处找。 */
function SpaceClosedNotice({ space }: { space: Space }) {
  return (
    <div className="p-3">
      <EmptyState
        title="空间还没打开。记忆只在空间运行时才读得到。"
        action={<OpenSpaceButton space={space} />}
      />
    </div>
  );
}

function OpenSpaceButton({ space }: { space: Space }) {
  const { open, opening } = useOpenSpace(
    space.ws_id,
    space.work_dir,
    space.team_id,
  );
  return (
    <Button size="sm" className="h-7" disabled={opening} onClick={open}>
      打开空间
    </Button>
  );
}
