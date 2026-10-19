import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useParams, useNavigate } from "react-router-dom";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { LoadingSpinner } from "@/components/common/LoadingSpinner";
import { PageHeader } from "@/components/common/PageHeader";
import { PageContainer } from "@/components/common/PageContainer";
import { MarkdownRenderer } from "@/components/chat/MarkdownRenderer";
import { ErrorBoundary } from "@/components/ErrorBoundary";
import {
  getSkill,
  listSkillFiles,
  readSkillFile,
  upsertSkill,
} from "@/api/skills";
import type { SkillDetail, SkillFile } from "@/types/skill";
import { AlertCircle, FolderTree, Lock } from "lucide-react";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { SkillFilePanel } from "@/components/skills/SkillFilePanel";
import { SkillFileTree } from "@/components/skills/SkillFileTree";
import { CodeEditor } from "@/components/skills/CodeEditor";
import { languageOf } from "@/lib/codeLanguage";
import { formatBytes } from "@/lib/format";
import { toast } from "sonner";

type ViewMode = "split" | "source" | "preview";

/** SKILL.md 在缓冲区表里的键，与后端清单中的资源文件路径天然不冲突。 */
const SKILL_MD = "SKILL.md";
/** 双栏下限：再窄源码栏只剩 74 字符/行，不如单栏给足宽度。 */
const SPLIT_MIN_WIDTH = 1080;
/** 常驻文件栏阈值：加 241px 栏后源码内容仍 ≥700 的最小容器宽。 */
const RAIL_MIN_WIDTH = 1600;
const PREVIEW_DEBOUNCE_MS = 250;

const VIEW_OPTIONS: { mode: ViewMode; label: string; testId: string }[] = [
  { mode: "split", label: "分栏", testId: "mode-split" },
  { mode: "source", label: "源码", testId: "mode-source" },
  { mode: "preview", label: "预览", testId: "mode-preview" },
];

/** 单个文件的编辑缓冲；`saved` 保留磁盘值，用于脏标记与保存后回写。 */
interface Doc {
  content: string;
  saved: string;
  truncated: boolean;
}

function PreviewError() {
  return (
    <div className="flex items-center gap-2 py-2 text-sm text-destructive">
      <AlertCircle className="h-4 w-4 shrink-0" />
      预览渲染失败，源码仍可编辑
    </div>
  );
}

export function SkillEditPage() {
  const { skillName } = useParams<{ skillName: string }>();
  const navigate = useNavigate();
  const [skill, setSkill] = useState<SkillDetail | null>(null);
  const [docs, setDocs] = useState<Record<string, Doc>>({});
  const [activePath, setActivePath] = useState(SKILL_MD);
  const [saving, setSaving] = useState(false);
  const [mode, setMode] = useState<ViewMode | null>(null);
  const [previewContent, setPreviewContent] = useState("");
  const [containerWidth, setContainerWidth] = useState(0);
  const [files, setFiles] = useState<SkillFile[]>([]);
  const [filesLoading, setFilesLoading] = useState(true);
  const [filesError, setFilesError] = useState(false);
  const [filesReloadKey, setFilesReloadKey] = useState(0);
  const [railCollapsed, setRailCollapsed] = useState(false);
  const [dirSheetOpen, setDirSheetOpen] = useState(false);
  const [fileLoading, setFileLoading] = useState(false);
  const [fileFailed, setFileFailed] = useState(false);
  const [fileReloadKey, setFileReloadKey] = useState(0);

  const containerRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const previewRef = useRef<HTMLDivElement>(null);
  const savedScroll = useRef({ source: 0, preview: 0 });
  const savedSelection = useRef<{ start: number; end: number } | null>(null);
  /** 每个文件各自的滚动位置与选区，切换回来时还原。 */
  const perFile = useRef<
    Record<string, { scroll: number; start: number; end: number }>
  >({});
  /** 本次进入文件是否已恢复过滚动/选区；`loaded` 标记内容是否已到位。 */
  const restored = useRef<{ path: string; loaded: boolean } | null>(null);

  const skillDoc = docs[SKILL_MD];
  const activeDoc = docs[activePath];

  useEffect(() => {
    if (!skillName) return;
    getSkill(skillName)
      .then((detail) => {
        setSkill(detail);
        setDocs({
          [SKILL_MD]: {
            content: detail.content,
            saved: detail.content,
            truncated: false,
          },
        });
        // 首屏直接渲染正文，避免防抖窗口里先闪一帧空态。
        setPreviewContent(detail.content);
      })
      .catch(() => toast.error("加载 Skill 失败"));
  }, [skillName]);

  useEffect(() => {
    if (!skillName) return;
    let cancelled = false;
    setFilesLoading(true);
    setFilesError(false);
    listSkillFiles(skillName)
      .then((data) => {
        if (!cancelled) setFiles(data.files);
      })
      .catch(() => {
        if (!cancelled) setFilesError(true);
      })
      .finally(() => {
        if (!cancelled) setFilesLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [skillName, filesReloadKey]);

  // 资源文件按需加载：只有真正打开时才请求，避免一次拉 19 个文件。
  // SKILL.md 由 getSkill 装载，不能走这里，否则会与首屏请求竞争覆盖缓冲。
  useEffect(() => {
    if (!skillName || activePath === SKILL_MD || activeDoc) return;
    let cancelled = false;
    setFileLoading(true);
    setFileFailed(false);
    readSkillFile(skillName, activePath)
      .then((data) => {
        if (cancelled) return;
        setDocs((prev) => ({
          ...prev,
          [data.path]: {
            content: data.content,
            saved: data.content,
            truncated: data.truncated,
          },
        }));
      })
      .catch(() => {
        if (!cancelled) setFileFailed(true);
      })
      .finally(() => {
        if (!cancelled) setFileLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [skillName, activePath, activeDoc, fileReloadKey]);

  // 容器宽决定默认视图与「分栏」是否可用；视口宽不可靠（侧栏展开/收起差 176px）。
  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    setContainerWidth(el.getBoundingClientRect().width);
    const observer = new ResizeObserver((entries) => {
      setContainerWidth(entries[0].contentRect.width);
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  // 预览只跟随 SKILL.md；编辑脚本时不需要渲染 Markdown。
  useEffect(() => {
    const content = skillDoc?.content ?? "";
    if (!content.trim()) {
      setPreviewContent(content);
      return;
    }
    const timer = window.setTimeout(
      () => setPreviewContent(content),
      PREVIEW_DEBOUNCE_MS,
    );
    return () => window.clearTimeout(timer);
  }, [skillDoc?.content]);

  const splitAvailable = containerWidth >= SPLIT_MIN_WIDTH;
  const railAvailable = containerWidth >= RAIL_MIN_WIDTH && !railCollapsed;
  const isSkillMd = activePath === SKILL_MD;
  const activeMode: ViewMode =
    mode === "split" && !splitAvailable
      ? "source"
      : (mode ?? (splitAvailable ? "split" : "source"));
  // 代码文件没有 Markdown 预览，源码独占。
  const effectiveMode: ViewMode = isSkillMd ? activeMode : "source";

  const writableOf = (path: string): boolean => {
    if (path === SKILL_MD) return true;
    return files.find((file) => file.path === path)?.writable ?? false;
  };
  const canEdit = writableOf(activePath) && !activeDoc?.truncated;
  const dirty = !!activeDoc && activeDoc.content !== activeDoc.saved;
  const dirtyPaths = Object.entries(docs)
    .filter(([, doc]) => doc.content !== doc.saved)
    .map(([path]) => path);
  const anyDirty = dirtyPaths.length > 0;

  // display:none 会把滚动位置归零，切换视图后按保存值写回。
  useLayoutEffect(() => {
    const source = textareaRef.current;
    if (source && effectiveMode !== "preview") {
      const selection = savedSelection.current;
      if (selection) source.setSelectionRange(selection.start, selection.end);
      source.scrollTop = savedScroll.current.source;
    }
    const preview = previewRef.current;
    if (preview && effectiveMode !== "source") {
      preview.scrollTop = savedScroll.current.preview;
    }
  }, [effectiveMode]);

  // 切换文件时记住上一份缓冲的滚动与选区，回来后原样恢复。
  // activeDoc 每次击键都是新对象，不能当作恢复的触发条件：同一文件只恢复一次。
  // 内容异步到位前 textarea 还没有对应文本，那次不记完成，等加载后再补一次。
  useLayoutEffect(() => {
    const source = textareaRef.current;
    const saved = perFile.current[activePath];
    if (!source || !saved) return;
    const done = restored.current;
    if (done?.path === activePath && (done.loaded || !activeDoc)) return;
    source.scrollTop = saved.scroll;
    source.setSelectionRange(saved.start, saved.end);
    restored.current = { path: activePath, loaded: Boolean(activeDoc) };
  }, [activePath, isSkillMd, activeDoc]);

  const rememberState = () => {
    const source = textareaRef.current;
    // 预览态 textarea 被 display:none，scrollTop 读出来恒为 0，不能拿它覆盖已记的位。
    if (source && effectiveMode !== "preview") {
      perFile.current[activePath] = {
        scroll: source.scrollTop,
        start: source.selectionStart,
        end: source.selectionEnd,
      };
      savedScroll.current.source = source.scrollTop;
      savedSelection.current = {
        start: source.selectionStart,
        end: source.selectionEnd,
      };
    } else if (!source && activePath === SKILL_MD) {
      savedScroll.current.source = 0;
    }
    const preview = previewRef.current;
    if (preview && effectiveMode !== "source") {
      savedScroll.current.preview = preview.scrollTop;
    }
  };

  useEffect(() => {
    if (!anyDirty) return;
    const handler = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", handler);
    return () => window.removeEventListener("beforeunload", handler);
  }, [anyDirty]);

  const handleSave = useCallback(async () => {
    if (!skillName || !activeDoc || !canEdit) return;
    if (isSkillMd && !activeDoc.content.trim()) return;
    setSaving(true);
    try {
      if (isSkillMd) {
        await upsertSkill(skillName, activeDoc.content);
      } else {
        // 带 SKILL.md 的磁盘版本：后端每次 upsert 都要校验 frontmatter，
        // 传 saved 而非草稿，避免存脚本时顺手提交未确认的 SKILL.md 改动。
        if (!skillDoc) throw new Error("SKILL.md 尚未加载");
        await upsertSkill(skillName, skillDoc.saved, [
          { path: activePath, content: activeDoc.content },
        ]);
      }
      setDocs((prev) => ({
        ...prev,
        [activePath]: { ...prev[activePath], saved: prev[activePath].content },
      }));
      toast.success(`${activePath} 已保存`);
    } catch {
      toast.error("保存失败");
    } finally {
      setSaving(false);
    }
  }, [activeDoc, activePath, canEdit, isSkillMd, skillDoc, skillName]);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.key.toLowerCase() !== "s") {
        return;
      }
      event.preventDefault();
      if (saving) return;
      void handleSave();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [handleSave, saving]);

  const switchMode = (next: ViewMode) => {
    if (next === "split" && !splitAvailable) return;
    rememberState();
    setMode(next);
  };

  const openFile = (file: SkillFile) => {
    if (file.kind === "binary") return;
    if (file.path === activePath) return;
    rememberState();
    restored.current = null;
    setActivePath(file.path);
  };

  const openSkillMd = () => {
    if (activePath === SKILL_MD) return;
    rememberState();
    restored.current = null;
    setActivePath(SKILL_MD);
  };

  const activeContent = activeDoc?.content ?? "";
  const lineCount = activeContent ? activeContent.split("\n").length : 0;
  const byteLength = new TextEncoder().encode(activeContent).length;
  const language = useMemo(
    () => (isSkillMd ? "markdown" : languageOf(activePath)),
    [activePath, isSkillMd],
  );

  const docMeta = `${activePath} · ${lineCount} 行 · ${formatBytes(byteLength)}`;

  return (
    <div ref={containerRef} className="@container flex h-full min-h-0 flex-col">
      <PageContainer className="flex min-h-0 flex-1 flex-col space-y-0 max-lg:h-auto max-lg:pb-6">
        {!skill ? (
          <LoadingSpinner />
        ) : (
          <>
            <header className="sticky -top-4 z-20 bg-background pb-4 md:-top-6 xl:-top-8">
              <PageHeader
                title={`编辑 Skill — ${skill.name}`}
                actions={
                  <div data-testid="action-bar" className="flex items-center gap-2">
                    {anyDirty && (
                      <span className="text-xs text-warning">
                        ●未保存
                        {!dirty && (
                          <span className="text-muted-foreground">
                            （当前文件已保存）
                          </span>
                        )}
                      </span>
                    )}
                    <Button
                      type="button"
                      variant="outline"
                      onClick={() => navigate("/workspace/skills")}
                    >
                      取消
                    </Button>
                    <Button
                      type="button"
                      onClick={() => void handleSave()}
                      disabled={!dirty || !canEdit || saving}
                      title={canEdit ? "保存当前文件（⌘S）" : "该文件不可写"}
                    >
                      {saving ? "保存中…" : "保存修改"}
                    </Button>
                  </div>
                }
              />
            </header>

            <div className="flex min-h-0 flex-1 overflow-hidden rounded-xl border bg-card">
              {railAvailable && (
                <SkillFilePanel
                  count={files.length + 1}
                  onCollapse={() => setRailCollapsed(true)}
                >
                  <SkillFileTree
                    files={files}
                    loading={filesLoading}
                    error={filesError}
                    onRetry={() => setFilesReloadKey((k) => k + 1)}
                    onOpenFile={openFile}
                    onOpenSkillMd={openSkillMd}
                    activePath={activePath}
                    dirtyPaths={dirtyPaths}
                  />
                </SkillFilePanel>
              )}

              <div className="flex min-w-0 flex-1 flex-col">
                <div className="flex h-11 shrink-0 items-center justify-between gap-3 border-b px-3">
                  <div className="flex min-w-0 items-center gap-2">
                    {!railAvailable && (
                      <Button
                        type="button"
                        size="sm"
                        variant="ghost"
                        className="h-7 gap-1 px-2 text-xs text-muted-foreground"
                        onClick={() => {
                          if (containerWidth >= RAIL_MIN_WIDTH) setRailCollapsed(false);
                          else setDirSheetOpen(true);
                        }}
                        aria-expanded={false}
                        aria-controls="skill-files"
                        title={`浏览文件（${files.length + 1}）`}
                        data-testid="file-tree-toggle"
                      >
                        <FolderTree className="size-3.5" />
                        文件
                      </Button>
                    )}
                    {isSkillMd ? (
                      <div
                        role="group"
                        aria-label="编辑器视图"
                        className="inline-flex rounded-lg bg-muted p-[3px]"
                      >
                        {VIEW_OPTIONS.map((option) => {
                          const disabled = option.mode === "split" && !splitAvailable;
                          return (
                            <Button
                              key={option.mode}
                              type="button"
                              size="xs"
                              variant={
                                activeMode === option.mode ? "secondary" : "ghost"
                              }
                              className="h-7 px-3"
                              data-testid={option.testId}
                              aria-pressed={activeMode === option.mode}
                              disabled={disabled}
                              title={disabled ? "窗口需要更宽" : undefined}
                              onClick={() => switchMode(option.mode)}
                            >
                              {option.label}
                            </Button>
                          );
                        })}
                      </div>
                    ) : null}
                    {!canEdit && (
                      <Badge variant="secondary" className="gap-1">
                        <Lock className="size-3" />
                        只读
                      </Badge>
                    )}
                  </div>
                  <span
                    data-testid="doc-meta"
                    className="hidden max-w-[50%] shrink-0 truncate text-xs text-muted-foreground tabular-nums @min-[520px]:inline"
                  >
                    {docMeta}
                  </span>
                </div>

                <div className="flex min-h-0 flex-1">
                  <div
                    data-pane="source"
                    hidden={effectiveMode === "preview"}
                    className="min-w-0 flex-1 overflow-hidden [scrollbar-gutter:stable]"
                  >
                    {fileLoading && !activeDoc ? (
                      <div className="space-y-2 px-5 py-4">
                        {[92, 78, 86, 64, 74].map((w) => (
                          <Skeleton key={w} className="h-3" style={{ width: `${w}%` }} />
                        ))}
                      </div>
                    ) : fileFailed && !activeDoc ? (
                      <div className="space-y-3 px-5 py-4">
                        <div className="flex items-center gap-1.5 text-sm text-destructive">
                          <AlertCircle className="size-4 shrink-0" />
                          读取 {activePath} 失败
                        </div>
                        <Button
                          type="button"
                          variant="outline"
                          size="sm"
                          onClick={() => setFileReloadKey((k) => k + 1)}
                        >
                          重试
                        </Button>
                      </div>
                    ) : activeDoc ? (
                      <div
                        className={
                          isSkillMd
                            ? "mx-auto h-full w-full max-w-[700px]"
                            : "h-full w-full"
                        }
                      >
                        <CodeEditor
                          key={activePath}
                          value={activeContent}
                          onChange={(next) =>
                            setDocs((prev) => ({
                              ...prev,
                              [activePath]: { ...prev[activePath], content: next },
                            }))
                          }
                          language={language}
                          readOnly={!canEdit}
                          ariaLabel={`${activePath} 内容`}
                          testId="code-editor"
                          textareaRef={textareaRef}
                        />
                      </div>
                    ) : null}
                  </div>

                  {isSkillMd && effectiveMode === "split" && (
                    <div className="w-px shrink-0 bg-border" />
                  )}

                  {isSkillMd && (
                    <div
                      data-pane="preview"
                      hidden={effectiveMode === "source"}
                      className="w-[clamp(520px,45%,600px)] shrink-0 overflow-y-auto bg-muted/30 px-5 py-4 [scrollbar-gutter:stable]"
                    >
                      <div className="mx-auto flex min-h-full w-full max-w-[560px] flex-col">
                        {previewContent.trim() ? (
                          <ErrorBoundary
                            key={previewContent}
                            fallback={<PreviewError />}
                          >
                            <MarkdownRenderer content={previewContent} />
                          </ErrorBoundary>
                        ) : (
                          <div className="my-auto max-w-[320px] self-center text-center text-sm text-muted-foreground">
                            <p>还没有内容</p>
                            <p className="mt-1">在左侧写 Markdown，这里会同步显示</p>
                          </div>
                        )}
                      </div>
                    </div>
                  )}
                </div>
              </div>
            </div>

            <Sheet open={dirSheetOpen} onOpenChange={setDirSheetOpen}>
              <SheetContent
                side="left"
                className="w-[280px] max-w-[85vw] gap-0 p-0"
                data-testid="file-tree-sheet"
              >
                <div className="flex h-11 shrink-0 items-center gap-2 border-b px-3 pr-10">
                  <SheetTitle className="text-xs font-medium">文件</SheetTitle>
                  <span className="ml-auto text-[11px] tabular-nums text-muted-foreground">
                    {files.length + 1}
                  </span>
                </div>
                <div className="min-h-0 flex-1 overflow-y-auto py-2 [overscroll-behavior:contain]">
                  <SkillFileTree
                    files={files}
                    loading={filesLoading}
                    error={filesError}
                    density="sheet"
                    testId="file-tree-sheet-body"
                    onRetry={() => setFilesReloadKey((k) => k + 1)}
                    onOpenFile={(file) => {
                      setDirSheetOpen(false);
                      openFile(file);
                    }}
                    onOpenSkillMd={() => {
                      setDirSheetOpen(false);
                      openSkillMd();
                    }}
                    activePath={activePath}
                    dirtyPaths={dirtyPaths}
                  />
                </div>
              </SheetContent>
            </Sheet>
          </>
        )}
      </PageContainer>
    </div>
  );
}
