import { useMemo, useRef, useState } from "react";
import {
  AlertCircle,
  Binary,
  ChevronRight,
  File,
  FileCode2,
  FileJson,
  FileQuestion,
  FileText,
  Lock,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";
import { formatBytes } from "@/lib/format";
import type { SkillFile } from "@/types/skill";

/** 服务端预览上限，超过则先询问再请求。 */
export const MAX_PREVIEW_BYTES = 256 * 1024;

export type FileGroupKey = "scripts" | "references" | "assets" | "other";

const GROUP_DEFS: { key: FileGroupKey; label: string; writable: boolean }[] = [
  { key: "scripts", label: "scripts/", writable: true },
  { key: "references", label: "references/", writable: true },
  { key: "assets", label: "assets/", writable: true },
  { key: "other", label: "其他", writable: false },
];

/** 分组 = 写权限边界：三个可写前缀之外、以及 __pycache__ 全归「其他」。 */
export function groupSkillFiles(
  files: SkillFile[],
): Record<FileGroupKey, SkillFile[]> {
  const groups: Record<FileGroupKey, SkillFile[]> = {
    scripts: [],
    references: [],
    assets: [],
    other: [],
  };
  for (const file of files) {
    const top = file.path.split("/")[0];
    if (
      file.path.includes("/") &&
      !file.path.includes("__pycache__/") &&
      (top === "scripts" || top === "references" || top === "assets")
    ) {
      groups[top].push(file);
    } else {
      groups.other.push(file);
    }
  }
  return groups;
}

/** 可写组内去掉组名前缀，其余保留完整相对路径。 */
export function displayName(path: string, group: FileGroupKey): string {
  if (group === "other") return path;
  return path.slice(group.length + 1);
}

function FileTypeIcon({ file }: { file: SkillFile }) {
  const name = file.path.split("/").pop() ?? file.path;
  const ext = name.includes(".") ? name.split(".").pop()!.toLowerCase() : "";
  const cls = "size-3.5 shrink-0";
  if (file.kind === "binary") return <Binary className={cls} />;
  if (file.kind === "env") return <Lock className={cls} />;
  if (file.kind === "markdown") return <FileText className={cls} />;
  if (file.kind === "code") return <FileCode2 className={cls} />;
  if (file.kind === "data") return <FileJson className={cls} />;
  if (!ext || ext === "txt" || ext === "log") return <File className={cls} />;
  return <FileQuestion className={cls} />;
}

interface SkillFileTreeProps {
  files: SkillFile[];
  loading: boolean;
  error: boolean;
  onRetry: () => void;
  onOpenFile: (file: SkillFile) => void;
  /** 点固定行切回 SKILL.md。 */
  onOpenSkillMd?: () => void;
  activePath?: string | null;
  /** 有未保存改动的文件路径，用于在行尾打点。 */
  dirtyPaths?: string[];
  /** rail 28px，sheet 32px（触屏目标）。 */
  density?: "rail" | "sheet";
  testId?: string;
}

type TreeRow =
  | { kind: "doc"; key: string }
  | { kind: "group"; key: string; group: FileGroupKey; expanded: boolean }
  | { kind: "file"; key: string; group: FileGroupKey; file: SkillFile };

export function SkillFileTree({
  files,
  loading,
  error,
  onRetry,
  onOpenFile,
  onOpenSkillMd,
  activePath,
  dirtyPaths = [],
  density = "rail",
  testId = "file-tree-body",
}: SkillFileTreeProps) {
  const [expanded, setExpanded] = useState<Record<FileGroupKey, boolean>>({
    scripts: true,
    references: true,
    assets: true,
    other: false,
  });
  const [focusKey, setFocusKey] = useState("skill-md");
  const nodeRefs = useRef(new Map<string, HTMLElement>());
  const rowH = density === "rail" ? "h-7" : "h-8";

  const groups = useMemo(() => groupSkillFiles(files), [files]);

  const nodes = useMemo<TreeRow[]>(() => {
    const list: TreeRow[] = [{ kind: "doc", key: "skill-md" }];
    for (const def of GROUP_DEFS) {
      const isOpen = expanded[def.key];
      list.push({
        kind: "group",
        key: `group:${def.key}`,
        group: def.key,
        expanded: isOpen,
      });
      if (isOpen) {
        for (const file of groups[def.key]) {
          list.push({ kind: "file", key: file.path, group: def.key, file });
        }
      }
    }
    return list;
  }, [groups, expanded]);

  const focusNode = (key: string) => {
    setFocusKey(key);
    nodeRefs.current.get(key)?.focus();
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    // 以事件源为准：焦点刚移动时 state 里的 focusKey 可能还没提交。
    const anchor = (event.target as HTMLElement | null)?.closest?.(
      "[data-node-key]",
    ) as HTMLElement | null;
    const anchorKey = anchor?.dataset.nodeKey ?? focusKey;
    const index = nodes.findIndex((n) => n.key === anchorKey);
    if (index < 0) return;
    const node = nodes[index];
    const move = (next: number) => {
      const clamped = Math.max(0, Math.min(nodes.length - 1, next));
      focusNode(nodes[clamped].key);
    };

    switch (event.key) {
      case "ArrowDown":
        move(index + 1);
        break;
      case "ArrowUp":
        move(index - 1);
        break;
      case "Home":
        move(0);
        break;
      case "End":
        move(nodes.length - 1);
        break;
      case "ArrowRight":
        if (node.kind === "group" && !node.expanded) {
          setExpanded((prev) => ({ ...prev, [node.group]: true }));
        } else {
          return;
        }
        break;
      case "ArrowLeft":
        if (node.kind === "group" && node.expanded) {
          setExpanded((prev) => ({ ...prev, [node.group]: false }));
        } else if (node.kind === "file") {
          focusNode(`group:${node.group}`);
        } else {
          return;
        }
        break;
      case "Enter":
      case " ": {
        if (node.kind === "group") {
          setExpanded((prev) => ({ ...prev, [node.group]: !prev[node.group] }));
        } else if (node.kind === "file") {
          if (node.file.kind === "binary") return;
          onOpenFile(node.file);
        } else if (node.kind === "doc") {
          onOpenSkillMd?.();
        } else {
          return;
        }
        break;
      }
      default:
        return;
    }
    event.preventDefault();
    event.stopPropagation();
  };

  if (error) {
    return (
      <div
        data-testid="file-tree-error"
        className="flex flex-col items-start gap-2 px-3 py-3"
      >
        <div className="flex items-center gap-1.5 text-xs text-destructive">
          <AlertCircle className="size-3.5 shrink-0" />
          目录加载失败
        </div>
        <Button
          type="button"
          size="xs"
          variant="ghost"
          className="h-6 px-2"
          onClick={onRetry}
        >
          重试
        </Button>
      </div>
    );
  }

  return (
    <div
      role="tree"
      aria-label="Skill 文件"
      aria-multiselectable="false"
      id="skill-files"
      data-testid={testId}
      onKeyDown={handleKeyDown}
      className="py-1"
    >
      <div
        role="treeitem"
        aria-level={1}
        aria-selected={activePath === "SKILL.md"}
        aria-current={activePath === "SKILL.md" ? "page" : undefined}
        data-node-key="skill-md"
        data-testid="file-row-skill-md"
        tabIndex={focusKey === "skill-md" ? 0 : -1}
        ref={(el) => {
          if (el) nodeRefs.current.set("skill-md", el);
          else nodeRefs.current.delete("skill-md");
        }}
        onFocus={() => setFocusKey("skill-md")}
        onClick={onOpenSkillMd}
        title="SKILL.md（当前文档）"
        className={cn(
          "relative mx-2 flex h-8 cursor-pointer items-center gap-1.5 rounded-md px-2 text-xs font-medium",
          "outline-hidden focus-visible:ring-2 focus-visible:ring-ring/60",
          activePath === "SKILL.md"
            ? "bg-accent/70 text-accent-foreground"
            : "text-muted-foreground hover:bg-accent/40 hover:text-foreground",
        )}
      >
        {activePath === "SKILL.md" && (
          <span className="absolute left-0 top-1/2 h-4 w-0.5 -translate-y-1/2 rounded-full bg-primary" />
        )}
        <FileText className="size-3.5 shrink-0" />
        <span className="truncate">SKILL.md</span>
        {dirtyPaths.includes("SKILL.md") && (
          <span
            aria-label="有未保存改动"
            className="ml-auto size-1.5 shrink-0 rounded-full bg-warning"
          />
        )}
      </div>

      {loading ? (
        <div data-testid="file-tree-skeleton" className="mt-1 space-y-1">
          {[100, 72, 88, 60].map((w) => (
            <div key={w} className="mx-2 flex h-7 items-center px-2">
              <Skeleton className="h-3" style={{ width: `${w}%` }} />
            </div>
          ))}
        </div>
      ) : (
        <>
          {GROUP_DEFS.map((def) => {
            const items = groups[def.key];
            const isOpen = expanded[def.key];
            const empty = items.length === 0;
            const groupKey = `group:${def.key}`;
            return (
              <div
                key={def.key}
                role="treeitem"
                aria-level={1}
                aria-expanded={empty ? undefined : isOpen}
                aria-disabled={empty || undefined}
                data-testid="file-group"
                data-group={def.key}
                data-node-key={groupKey}
                tabIndex={empty ? -1 : focusKey === groupKey ? 0 : -1}
                ref={(el) => {
                  if (el) nodeRefs.current.set(groupKey, el);
                  else nodeRefs.current.delete(groupKey);
                }}
                onFocus={() => setFocusKey(groupKey)}
                className="outline-hidden"
              >
                <div
                  onClick={(event) => {
                    event.stopPropagation();
                    if (empty) return;
                    // 折叠会把聚焦中的子行卸载，先把焦点收回到组头。
                    if (isOpen) focusNode(`group:${def.key}`);
                    setExpanded((prev) => ({ ...prev, [def.key]: !prev[def.key] }));
                  }}
                  title={empty ? "暂无文件" : undefined}
                  className={cn(
                    "mx-2 flex h-7 items-center gap-1 rounded-md px-2 text-[11px] font-medium",
                    "text-muted-foreground",
                    empty
                      ? "cursor-default opacity-60"
                      : "cursor-pointer hover:bg-accent/40",
                    "peer-focus-visible:ring-2",
                  )}
                >
                  {empty ? (
                    <span className="w-3 shrink-0" />
                  ) : (
                    <ChevronRight
                      className={cn(
                        "size-3 shrink-0 transition-transform",
                        isOpen && "rotate-90",
                      )}
                    />
                  )}
                  <span className="truncate">{def.label}</span>
                  {!def.writable && (
                    <span
                      className="ml-auto text-[10px] text-muted-foreground/70"
                      title="这些路径不可保存（写权限仅 scripts/ references/ assets/）"
                    >
                      只读
                    </span>
                  )}
                  <span
                    className={cn(
                      "tabular-nums",
                      def.writable ? "ml-auto" : "ml-1",
                    )}
                  >
                    {items.length}
                  </span>
                </div>

                {isOpen && !empty && (
                  <div role="group">
                    {items.map((file) => {
                      const binary = file.kind === "binary";
                      const oversize = file.size > MAX_PREVIEW_BYTES;
                      const active = activePath === file.path;
                      return (
                        <div
                          key={file.path}
                          role="treeitem"
                          aria-level={2}
                          aria-disabled={binary || undefined}
                          aria-current={active ? "true" : undefined}
                          data-testid="file-row"
                          data-path={file.path}
                          data-group={def.key}
                          data-node-key={file.path}
                          tabIndex={focusKey === file.path ? 0 : -1}
                          ref={(el) => {
                            if (el) nodeRefs.current.set(file.path, el);
                            else nodeRefs.current.delete(file.path);
                          }}
                          onFocus={() => setFocusKey(file.path)}
                          onClick={(event) => {
                            event.stopPropagation();
                            if (binary) return;
                            onOpenFile(file);
                          }}
                          title={binary ? "二进制文件，不可预览" : file.path}
                          className={cn(
                            "mx-2 flex items-center gap-1.5 rounded-md px-2 text-xs transition-colors",
                            "text-muted-foreground outline-hidden focus-visible:ring-2 focus-visible:ring-ring/60",
                            rowH,
                            binary
                              ? "cursor-default opacity-50"
                              : "cursor-pointer hover:bg-accent/50 hover:text-foreground",
                            active && "bg-accent/50 text-foreground",
                          )}
                        >
                          <FileTypeIcon file={file} />
                          <span className="truncate">
                            {displayName(file.path, def.key)}
                          </span>
                          {binary && (
                            <span className="sr-only">，二进制，不可预览</span>
                          )}
                          {dirtyPaths.includes(file.path) && (
                            <span
                              aria-label="有未保存改动"
                              className={cn(
                                "size-1.5 shrink-0 rounded-full bg-warning",
                                !oversize && "ml-auto",
                              )}
                            />
                          )}
                          {oversize && !binary && (
                            <span className="ml-auto shrink-0 text-[10px] tabular-nums text-warning">
                              &gt;{formatBytes(MAX_PREVIEW_BYTES)}
                            </span>
                          )}
                        </div>
                      );
                    })}
                  </div>
                )}
              </div>
            );
          })}
          {files.length === 0 && (
            <div
              data-testid="file-tree-empty"
              className="px-3 py-2 text-[11px] leading-4 text-muted-foreground"
            >
              scripts/、references/、assets/ 里还没有文件
            </div>
          )}
        </>
      )}
    </div>
  );
}