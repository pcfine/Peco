// 文件 tab：只读浏览。没有编辑与删除入口 —— 改动回到磁盘上的编辑器，
// 这里只回答「这个目录里有什么」和「刚有哪些变了」。

import { useEffect, useRef, useState } from "react";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { MarkdownRenderer } from "@/components/chat/MarkdownRenderer";
import { SPACE_FILES_POLL_MS, useSpaceFiles } from "@/queries/spaceDetail";
import { errorText } from "@/lib/api";
import {
  diffEntries,
  FILE_HIGHLIGHT_MS,
  snapshotEntries,
  type FileDirSnapshot,
  type FileEntryMark,
} from "@/lib/filesSnapshot";
import { formatBytes } from "@/lib/format";
import { cn } from "@/lib/utils";

function join(dir: string, name: string): string {
  return dir ? `${dir}/${name}` : name;
}

/** 高亮按「目录 + 名字」分键：同名文件出现在不同目录里互不相干。 */
function markKey(dir: string, name: string): string {
  return `${dir}\u0000${name}`;
}

const MARK_LABELS: Record<FileEntryMark, string> = {
  new: "新增",
  updated: "已更新",
};

/** 非颜色信号：色块之外还有一个字形，只靠背景色区分不成立。 */
const MARK_GLYPHS: Record<FileEntryMark, string> = { new: "+", updated: "~" };

function Breadcrumb({
  dir,
  onNavigate,
}: {
  dir: string;
  onNavigate: (next: string) => void;
}) {
  const segments = dir ? dir.split("/") : [];
  return (
    <div className="flex h-8 shrink-0 items-center gap-1 overflow-x-auto border-b border-line px-2 font-mono text-caption font-medium">
      <button
        type="button"
        onClick={() => onNavigate("")}
        className="tap-target -mx-2 shrink-0 px-2 text-ink-muted hover:text-ink"
      >
        根目录
      </button>
      {segments.map((segment, index) => (
        <span
          key={segment + index}
          className="flex shrink-0 items-center gap-1"
        >
          <span className="text-ink-faint">/</span>
          <button
            type="button"
            onClick={() => onNavigate(segments.slice(0, index + 1).join("/"))}
            className="text-ink-muted hover:text-ink"
          >
            {segment}
          </button>
        </span>
      ))}
    </div>
  );
}

export function SpaceFilesTab({
  wsId,
  active = true,
}: {
  wsId: string;
  /** 面板是否真的在看：隐藏或切到别的模式时停轮询。 */
  active?: boolean;
}) {
  const [dir, setDir] = useState("");
  const [openPath, setOpenPath] = useState<string | null>(null);

  const pollMs = active ? SPACE_FILES_POLL_MS : false;
  const listing = useSpaceFiles(wsId, dir, {
    enabled: openPath === null,
    refetchInterval: pollMs,
  });
  // 预览也跟着轮：文件可能在编辑器里被改过，用户重新点开前该看到新内容。
  const preview = useSpaceFiles(wsId, openPath ?? "", {
    enabled: openPath !== null,
    refetchInterval: pollMs,
  });

  const dirData =
    listing.data && "entries" in listing.data ? listing.data : null;
  const fileData = preview.data && "kind" in preview.data ? preview.data : null;
  const openName = openPath?.split("/").pop() ?? "";

  // 每个目录的上一份指纹与当前高亮。指纹留在 ref 里：它只用于比对，
  // 变了不必重渲染；高亮本身进 state，因为到期要换掉界面。
  const snapshots = useRef(new Map<string, FileDirSnapshot>());
  const expiry = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const [marks, setMarks] = useState<ReadonlyMap<string, FileEntryMark>>(
    new Map(),
  );

  useEffect(() => {
    // 换目录时会先挂着上一个目录的列表（placeholderData），那份数据不能拿来
    // 与当前目录的指纹比对，否则整屏都会判成「新增」。
    if (!dirData || listing.isPlaceholderData) return;

    const fresh = diffEntries(snapshots.current.get(dir), dirData.entries);
    snapshots.current.set(dir, snapshotEntries(dirData.entries));
    if (fresh.size === 0) return;

    setMarks((prev) => {
      const next = new Map(prev);
      for (const [name, mark] of fresh) next.set(markKey(dir, name), mark);
      return next;
    });
    for (const name of fresh.keys()) {
      const key = markKey(dir, name);
      clearTimeout(expiry.current.get(key));
      expiry.current.set(
        key,
        setTimeout(() => {
          expiry.current.delete(key);
          setMarks((prev) => {
            if (!prev.has(key)) return prev;
            const next = new Map(prev);
            next.delete(key);
            return next;
          });
        }, FILE_HIGHLIGHT_MS),
      );
    }
  }, [dir, dirData, listing.isPlaceholderData]);

  useEffect(() => {
    const timers = expiry.current;
    return () => {
      for (const timer of timers.values()) clearTimeout(timer);
      timers.clear();
    };
  }, []);

  const openDir = (next: string) => {
    setOpenPath(null);
    setDir(next);
  };

  return (
    <div className="flex h-full min-h-0 flex-col @min-[768px]/work:flex-row">
      <div
        className={cn(
          "flex min-h-0 flex-col border-line @min-[768px]/work:w-60 @min-[768px]/work:shrink-0 @min-[768px]/work:border-r",
          openPath !== null && "@max-[767px]/work:hidden",
        )}
      >
        <Breadcrumb dir={dir} onNavigate={openDir} />
        <div className="min-h-0 flex-1 overflow-y-auto">
          {listing.error && (
            <div className="p-3">
              <ErrorBanner
                message={errorText(listing.error)}
                onRetry={() => void listing.refetch()}
              />
            </div>
          )}
          {listing.isPending && (
            <div className="space-y-px p-3">
              {[0, 1, 2].map((row) => (
                <Skeleton key={row} className="h-7 w-full" />
              ))}
            </div>
          )}
          {dirData && dirData.entries.length === 0 && (
            <div className="p-3">
              <EmptyState title="这个目录是空的。" />
            </div>
          )}
          <ul>
            {(dirData?.entries ?? []).map((entry) => {
              const mark = marks.get(markKey(dir, entry.name));
              return (
                <li key={entry.name}>
                  <button
                    type="button"
                    onClick={() =>
                      entry.kind === "dir"
                        ? openDir(join(dir, entry.name))
                        : setOpenPath(join(dir, entry.name))
                    }
                    title={mark ? MARK_LABELS[mark] : undefined}
                    data-highlight={mark}
                    className={cn(
                      "flex h-7 w-full items-center gap-2 pl-2 pr-3 text-left font-mono text-mono-sm hover:bg-line/30",
                      entry.kind === "dir" ? "text-ink" : "text-ink-muted",
                      mark &&
                        "bg-primary-ink/10 shadow-[inset_2px_0_0_var(--color-primary-ink)]",
                    )}
                  >
                    <span className="w-3 shrink-0 text-2xs text-ink-faint">
                      {entry.kind === "dir" ? "▸" : "·"}
                    </span>
                    {/* 定宽槽位：标记出现/消退时整行不跟着位移。 */}
                    <span
                      aria-hidden="true"
                      className="w-2 shrink-0 text-center font-mono text-2xs text-primary-ink"
                    >
                      {mark ? MARK_GLYPHS[mark] : ""}
                    </span>
                    <span className="min-w-0 flex-1 truncate">
                      {entry.name}
                    </span>
                    {mark && (
                      <span className="sr-only">{MARK_LABELS[mark]}</span>
                    )}
                    {entry.kind === "file" && (
                      <span className="shrink-0 tabular-nums text-2xs text-ink-faint">
                        {formatBytes(entry.size_bytes ?? 0)}
                      </span>
                    )}
                  </button>
                </li>
              );
            })}
          </ul>
          {dirData?.truncated && (
            <p className="px-3 py-2 text-caption font-medium text-ink-faint">
              这个目录的条目超过 1000，只列出前 1000 个。
            </p>
          )}
        </div>
      </div>

      <div
        className={cn(
          "flex min-h-0 min-w-0 flex-1 flex-col",
          openPath === null && "@max-[767px]/work:hidden",
        )}
      >
        {openPath === null ? (
          <div className="p-3">
            <EmptyState title="选一个文件查看内容。" />
          </div>
        ) : (
          <>
            <div className="flex h-8 shrink-0 items-center gap-2 border-b border-line px-2">
              <button
                type="button"
                onClick={() => setOpenPath(null)}
                className="shrink-0 text-caption font-medium text-ink-muted hover:text-ink @min-[768px]/work:hidden"
              >
                ← 返回目录
              </button>
              <span className="min-w-0 flex-1 truncate font-mono text-2xs text-ink-muted">
                {openPath}
              </span>
              <span className="shrink-0 tabular-nums text-2xs text-ink-faint">
                {fileData ? formatBytes(fileData.size_bytes) : ""}
              </span>
            </div>
            <div className="min-h-0 flex-1 overflow-y-auto p-3">
              {preview.error && (
                <ErrorBanner message={errorText(preview.error)} />
              )}
              {preview.isPending && <Skeleton className="h-16 w-full" />}
              {fileData?.binary && (
                <p className="max-w-narrow text-sm text-ink-muted">
                  这是二进制文件，不在这里渲染。
                </p>
              )}
              {fileData?.text !== undefined &&
                (openName.endsWith(".md") ? (
                  <MarkdownRenderer content={fileData.text} />
                ) : (
                  <pre className="whitespace-pre-wrap break-words font-mono text-mono-sm text-ink">
                    {fileData.text}
                  </pre>
                ))}
              {fileData?.truncated && (
                <p className="mt-2 text-caption font-medium text-ink-faint">
                  只显示前 256 KB，完整内容请到目录里打开。
                </p>
              )}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
