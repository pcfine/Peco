import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { useParams, useNavigate } from "react-router-dom";
import { Button } from "@/components/ui/button";
import { LoadingSpinner } from "@/components/common/LoadingSpinner";
import { PageHeader } from "@/components/common/PageHeader";
import { PageContainer } from "@/components/common/PageContainer";
import { MarkdownRenderer } from "@/components/chat/MarkdownRenderer";
import { ErrorBoundary } from "@/components/ErrorBoundary";
import { getSkill, upsertSkill } from "@/api/skills";
import type { SkillDetail } from "@/types/skill";
import { AlertCircle } from "lucide-react";
import { toast } from "sonner";

type ViewMode = "split" | "source" | "preview";

/** 双栏下限：再窄源码栏只剩 74 字符/行，不如单栏给足宽度。 */
const SPLIT_MIN_WIDTH = 1080;
const PREVIEW_DEBOUNCE_MS = 250;

const VIEW_OPTIONS: { mode: ViewMode; label: string; testId: string }[] = [
  { mode: "split", label: "分栏", testId: "mode-split" },
  { mode: "source", label: "源码", testId: "mode-source" },
  { mode: "preview", label: "预览", testId: "mode-preview" },
];

function PreviewError() {
  return (
    <div className="flex items-center gap-2 py-2 text-sm text-destructive">
      <AlertCircle className="h-4 w-4 shrink-0" />
      预览渲染失败，源码仍可编辑
    </div>
  );
}

function formatCharCount(length: number): string {
  return length >= 1000 ? `${(length / 1000).toFixed(1)}k` : `${length}`;
}

export function SkillEditPage() {
  const { skillName } = useParams<{ skillName: string }>();
  const navigate = useNavigate();
  const [skill, setSkill] = useState<SkillDetail | null>(null);
  const [content, setContent] = useState("");
  const [saving, setSaving] = useState(false);
  const [mode, setMode] = useState<ViewMode | null>(null);
  const [previewContent, setPreviewContent] = useState("");
  const [containerWidth, setContainerWidth] = useState(0);

  const containerRef = useRef<HTMLDivElement>(null);
  const sourceRef = useRef<HTMLTextAreaElement>(null);
  const previewRef = useRef<HTMLDivElement>(null);
  const savedScroll = useRef({ source: 0, preview: 0 });
  const savedSelection = useRef<{ start: number; end: number } | null>(null);

  useEffect(() => {
    if (!skillName) return;
    getSkill(skillName)
      .then((detail) => {
        setSkill(detail);
        setContent(detail.content);
        // 首屏直接渲染正文，避免防抖窗口里先闪一帧空态。
        setPreviewContent(detail.content);
      })
      .catch(() => toast.error("加载 Skill 失败"));
  }, [skillName]);

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

  // 空内容立即渲染空态，其余防抖，避免每键重排整棵 Markdown 树。
  useEffect(() => {
    if (!content.trim()) {
      setPreviewContent(content);
      return;
    }
    const timer = window.setTimeout(
      () => setPreviewContent(content),
      PREVIEW_DEBOUNCE_MS,
    );
    return () => window.clearTimeout(timer);
  }, [content]);

  const splitAvailable = containerWidth >= SPLIT_MIN_WIDTH;
  const activeMode: ViewMode =
    mode === "split" && !splitAvailable
      ? "source"
      : (mode ?? (splitAvailable ? "split" : "source"));

  // display:none 会把滚动位置归零，切换后按保存值写回。
  useLayoutEffect(() => {
    const source = sourceRef.current;
    if (source && activeMode !== "preview") {
      const selection = savedSelection.current;
      if (selection) source.setSelectionRange(selection.start, selection.end);
      source.scrollTop = savedScroll.current.source;
    }
    const preview = previewRef.current;
    if (preview && activeMode !== "source") {
      preview.scrollTop = savedScroll.current.preview;
    }
  }, [activeMode]);

  const dirty = skill !== null && content.trim() !== skill.content.trim();

  useEffect(() => {
    if (!dirty) return;
    const handler = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", handler);
    return () => window.removeEventListener("beforeunload", handler);
  }, [dirty]);

  const handleSave = useCallback(async () => {
    if (!skill || !content.trim()) return;
    setSaving(true);
    try {
      await upsertSkill(skill.name, content);
      toast.success("保存成功");
      navigate("/workspace/skills");
    } catch {
      toast.error("保存失败");
    } finally {
      setSaving(false);
    }
  }, [skill, content, navigate]);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (
        !(event.metaKey || event.ctrlKey) ||
        event.key.toLowerCase() !== "s"
      ) {
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
    const source = sourceRef.current;
    if (source && activeMode !== "preview") {
      savedScroll.current.source = source.scrollTop;
      savedSelection.current = {
        start: source.selectionStart,
        end: source.selectionEnd,
      };
    }
    const preview = previewRef.current;
    if (preview && activeMode !== "source") {
      savedScroll.current.preview = preview.scrollTop;
    }
    setMode(next);
  };

  const lineCount = content ? content.split("\n").length : 0;

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
                  <div
                    data-testid="action-bar"
                    className="flex items-center gap-2"
                  >
                    {dirty && (
                      <span className="text-xs text-warning">●未保存</span>
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
                      disabled={!content.trim() || saving}
                      title="保存修改（⌘S）"
                    >
                      {saving ? "保存中…" : "保存修改"}
                    </Button>
                  </div>
                }
              />
            </header>

            <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-xl border bg-card">
              <div className="flex h-11 shrink-0 items-center justify-between gap-3 border-b px-3">
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
                <span className="hidden shrink-0 text-xs text-muted-foreground tabular-nums @min-[520px]:inline">
                  SKILL.md · {lineCount} 行 · {formatCharCount(content.length)}
                </span>
              </div>

              <div className="flex min-h-0 flex-1">
                <div
                  data-pane="source"
                  hidden={activeMode === "preview"}
                  className="min-w-0 flex-1 overflow-y-auto px-5 py-4 [scrollbar-gutter:stable]"
                >
                  <div className="h-full w-full max-w-[700px]">
                    <textarea
                      ref={sourceRef}
                      aria-label="SKILL.md 内容"
                      spellCheck={false}
                      value={content}
                      onChange={(event) => setContent(event.target.value)}
                      className="h-full w-full resize-none rounded-none border-0 bg-transparent px-0 py-0 font-mono text-sm leading-5 outline-none field-sizing-fixed focus-visible:ring-0"
                    />
                  </div>
                </div>

                {activeMode === "split" && (
                  <div className="w-px shrink-0 bg-border" />
                )}

                <div
                  data-pane="preview"
                  hidden={activeMode === "source"}
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
                        <p className="mt-1">
                          在左侧写 Markdown，这里会同步显示
                        </p>
                      </div>
                    )}
                  </div>
                </div>
              </div>
            </div>
          </>
        )}
      </PageContainer>
    </div>
  );
}
