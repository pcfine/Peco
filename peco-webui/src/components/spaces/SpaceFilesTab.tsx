// 文件 tab：只读浏览。没有编辑与删除入口 —— 改动回到磁盘上的编辑器，
// 这里只回答「这个目录里有什么」。

import { useState } from "react";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { MarkdownRenderer } from "@/components/chat/MarkdownRenderer";
import { useSpaceFiles } from "@/queries/spaceDetail";
import { errorText } from "@/lib/api";
import { formatBytes } from "@/lib/format";
import { cn } from "@/lib/utils";

function join(dir: string, name: string): string {
  return dir ? `${dir}/${name}` : name;
}

function Breadcrumb({
  dir,
  onNavigate,
}: {
  dir: string;
  onNavigate: (next: string) => void;
}) {
  const segments = dir ? dir.split("/") : [];
  return (
    <div className="flex h-8 shrink-0 items-center gap-1 overflow-x-auto border-b border-line px-2 font-mono text-2xs">
      <button
        type="button"
        onClick={() => onNavigate("")}
        className="shrink-0 text-ink-muted hover:text-ink"
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

export function SpaceFilesTab({ wsId }: { wsId: string }) {
  const [dir, setDir] = useState("");
  const [openPath, setOpenPath] = useState<string | null>(null);

  const listing = useSpaceFiles(wsId, dir, { enabled: openPath === null });
  const preview = useSpaceFiles(wsId, openPath ?? "", {
    enabled: openPath !== null,
  });

  const dirData =
    listing.data && "entries" in listing.data ? listing.data : null;
  const fileData = preview.data && "kind" in preview.data ? preview.data : null;
  const openName = openPath?.split("/").pop() ?? "";

  const openDir = (next: string) => {
    setOpenPath(null);
    setDir(next);
  };

  return (
    <div className="flex h-full min-h-0 flex-col @min-[768px]/main:flex-row">
      <div
        className={cn(
          "flex min-h-0 flex-col border-line @min-[768px]/main:w-60 @min-[768px]/main:shrink-0 @min-[768px]/main:border-r",
          openPath !== null && "@max-[767px]/main:hidden",
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
            {(dirData?.entries ?? []).map((entry) => (
              <li key={entry.name}>
                <button
                  type="button"
                  onClick={() =>
                    entry.kind === "dir"
                      ? openDir(join(dir, entry.name))
                      : setOpenPath(join(dir, entry.name))
                  }
                  className={cn(
                    "flex h-7 w-full items-center gap-2 pl-2 pr-3 text-left font-mono text-mono-sm hover:bg-line/30",
                    entry.kind === "dir" ? "text-ink" : "text-ink-muted",
                  )}
                >
                  <span className="w-3 shrink-0 text-2xs text-ink-faint">
                    {entry.kind === "dir" ? "▸" : "·"}
                  </span>
                  <span className="min-w-0 flex-1 truncate">{entry.name}</span>
                  {entry.kind === "file" && (
                    <span className="shrink-0 tabular-nums text-2xs text-ink-faint">
                      {formatBytes(entry.size_bytes ?? 0)}
                    </span>
                  )}
                </button>
              </li>
            ))}
          </ul>
          {dirData?.truncated && (
            <p className="px-3 py-2 text-2xs text-ink-faint">
              这个目录的条目超过 1000，只列出前 1000 个。
            </p>
          )}
        </div>
      </div>

      <div
        className={cn(
          "flex min-h-0 min-w-0 flex-1 flex-col",
          openPath === null && "@max-[767px]/main:hidden",
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
                className="shrink-0 text-2xs text-ink-muted hover:text-ink @min-[768px]/main:hidden"
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
                <p className="max-w-[480px] text-sm text-ink-muted">
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
                <p className="mt-2 text-2xs text-ink-faint">
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
