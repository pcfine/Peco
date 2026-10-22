// 会话产物面板：标题计数 + 分类过滤 + 卡片列表 + 加载更多。

import { useEffect, useMemo, useRef, useState } from "react";
import { AlertTriangle, Loader2, Package } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";
import {
  mergeArtifacts,
  usePecoArtifactStore,
  type ArtifactFilter,
} from "@/stores/pecoArtifactStore";
import type { ArtifactItem } from "@/types/artifact";
import { ArtifactCard } from "./ArtifactCard";
import { ArtifactPreviewDialog } from "./ArtifactPreviewDialog";

const FILTERS: { value: ArtifactFilter; label: string }[] = [
  { value: "all", label: "全部" },
  { value: "image", label: "图片" },
  { value: "video", label: "视频" },
  { value: "audio", label: "音频" },
  { value: "document", label: "文档" },
];

type FilterValue = ArtifactFilter;

export function ArtifactPanel() {
  // 逐个字段订阅：整 store 订阅会让 loading/error 的每次翻转都重建 items，
  // 整列卡片跟着重渲染。两个来源数组的引用只在真正新增/改写时变。
  const ingested = usePecoArtifactStore((s) => s.ingested);
  const pages = usePecoArtifactStore((s) => s.pages);
  const total = usePecoArtifactStore((s) => s.total);
  const loading = usePecoArtifactStore((s) => s.loading);
  const loadingMore = usePecoArtifactStore((s) => s.loadingMore);
  const error = usePecoArtifactStore((s) => s.error);
  const loaded = usePecoArtifactStore((s) => s.loaded);
  const nextOffset = usePecoArtifactStore((s) => s.nextOffset);
  const load = usePecoArtifactStore((s) => s.load);
  const loadMore = usePecoArtifactStore((s) => s.loadMore);
  const refreshHead = usePecoArtifactStore((s) => s.refreshHead);
  const filter = usePecoArtifactStore((s) => s.filter);
  const setFilter = usePecoArtifactStore((s) => s.setFilter);

  const items = useMemo(
    () => mergeArtifacts(ingested, pages),
    [ingested, pages],
  );

  const [previewItem, setPreviewItem] = useState<ArtifactItem | null>(null);
  /** 打开当前预览的卡片按钮：查看器挂在面板层，关闭时靠它把焦点还回卡片。 */
  const previewTriggerRef = useRef<HTMLButtonElement | null>(null);
  /** 触发按钮已置灰时的焦点落点：落在逻辑容器上，Tab 仍在面板内按原序移动。 */
  const panelRef = useRef<HTMLDivElement>(null);
  const scrollHostRef = useRef<HTMLDivElement>(null);
  const [scrollRoot, setScrollRoot] = useState<HTMLElement | null>(null);

  useEffect(() => {
    // 模块级 store 跨路由存活，load() 自己在 loaded 时短路
    if (!loaded) void load();
  }, [load, loaded]);

  useEffect(() => {
    // 视口观察的 root 必须是面板自己的滚动容器：按窗口判断的话，卡片滚出面板
    // 可视区仍算"在窗口内"，release 永不触发，缓存上界随之失效
    const viewport = scrollHostRef.current?.querySelector<HTMLElement>(
      '[data-slot="scroll-area-viewport"]',
    );
    setScrollRoot(viewport ?? null);
  }, []);

  const counts = useMemo(() => {
    const byKind: Record<FilterValue, number> = {
      all: items.length,
      image: 0,
      video: 0,
      audio: 0,
      document: 0,
    };
    for (const item of items) {
      if (item.kind === "image") byKind.image += 1;
      else if (item.kind === "video") byKind.video += 1;
      else if (item.kind === "audio") byKind.audio += 1;
      else if (item.kind === "document") byKind.document += 1;
    }
    return byKind;
  }, [items]);

  const openPreview = (next: ArtifactItem, trigger: HTMLButtonElement) => {
    // 每次都覆盖：先后预览两张卡时，归还目标必须是最后按下的那一个
    previewTriggerRef.current = trigger;
    setPreviewItem(next);
  };

  const visible = useMemo(
    () => (filter === "all" ? items : items.filter((i) => i.kind === filter)),
    [filter, items],
  );

  const allUnavailable =
    items.length > 0 && items.every((item) => item.available === false);
  const showSkeletons = loading && !loaded;
  const showEmpty = loaded && total === 0;
  const showLoadError = error !== null && items.length === 0;

  return (
    <div
      ref={panelRef}
      // 兜底焦点落点自己要有名字：这个 group 没名字时 Chromium 给的 accessible name
      // 是空的，播报什么就退给 AT 从子树里自己取，各家不一致。不用 aria-labelledby
      // 指 h3，是因为 h3 文本带数量（「产物 · N」），数量一变名字就变。
      // 与抽屉的无障碍名字（sr-only 的 SheetTitle）只差一个前缀是刻意接受的：group
      // 不是 landmark、不进地标列表，只在一次播报里重复一次名字 —— 不要为了"去重"把它改掉
      role="group"
      aria-label="对话产物"
      tabIndex={-1}
      className={cn(
        // tabIndex=-1 的容器只被程序化聚焦（永不进 Tab 序列），焦点环用 `focus:`
        // 而不是 `focus-visible:`：鼠标关闭查看器时引擎不判定 :focus-visible，
        // 键盘用户就看不到焦点落在哪了
        // 浓度 /60 与卡片的全色 `focus-visible:ring-ring` 不同是刻意的：这里是 2px
        // 整块 inset 环、面积远大于卡片内框，全色过重
        "flex min-h-0 flex-1 flex-col gap-2 px-4 pt-3 pb-4 focus:ring-2 focus:ring-ring/60 focus:ring-inset focus:outline-hidden",
      )}
    >
      <h3 className="px-1 text-sm font-semibold">产物 · {total}</h3>

      <Tabs
        value={filter}
        onValueChange={(next) => setFilter(next as FilterValue)}
        className="flex min-h-0 flex-1 flex-col gap-2"
      >
        <TabsList className="w-full shrink-0">
          {FILTERS.map((item) => (
            <TabsTrigger
              key={item.value}
              value={item.value}
              disabled={counts[item.value] === 0}
              className="text-xs"
            >
              {item.label}
              <span aria-hidden="true">{counts[item.value]}</span>
            </TabsTrigger>
          ))}
        </TabsList>

        <TabsContent value={filter} className="min-h-0 flex-1">
          <div ref={scrollHostRef} className="h-full">
            <ScrollArea className="h-full">
              <div className="flex flex-col gap-3 px-1 pb-3">
                {error !== null && items.length > 0 ? (
                  <p
                    role="status"
                    className="flex items-center gap-1.5 text-[11px] text-muted-foreground"
                  >
                    刷新失败，显示的是上次结果
                    <button
                      type="button"
                      onClick={() => void refreshHead()}
                      className="cursor-pointer text-primary underline underline-offset-2"
                    >
                      重试
                    </button>
                  </p>
                ) : null}

                {allUnavailable ? (
                  <p
                    role="status"
                    className="rounded-md border border-warning/25 bg-warning/15 px-2 py-1.5 text-[11px] text-warning"
                  >
                    已加载的产物中，这些文件已被清理
                  </p>
                ) : null}

                {showSkeletons
                  ? [0, 1, 2].map((i) => (
                      <div
                        key={i}
                        data-testid="artifact-skeleton"
                        className="rounded-lg border p-2.5"
                      >
                        <Skeleton className="h-32 w-full" />
                        <Skeleton className="mt-2 h-3 w-3/4" />
                        <Skeleton className="mt-1.5 h-3 w-1/2" />
                      </div>
                    ))
                  : null}

                {showEmpty ? (
                  <div className="flex flex-col items-center gap-2 px-3 py-10 text-center">
                    <Package
                      className="h-8 w-8 text-muted-foreground"
                      aria-hidden="true"
                    />
                    <p className="text-sm font-medium">还没有产物</p>
                    <p className="text-xs text-muted-foreground">
                      我生成的图片、视频、音频和文档会出现在这里
                    </p>
                  </div>
                ) : null}

                {showLoadError ? (
                  <div className="flex flex-col items-center gap-2 px-3 py-10 text-center">
                    <AlertTriangle
                      className="h-8 w-8 text-destructive"
                      aria-hidden="true"
                    />
                    <p className="text-sm">产物加载失败</p>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => void load()}
                    >
                      重试
                    </Button>
                  </div>
                ) : null}

                {visible.map((item) => (
                  <ArtifactCard
                    key={item.id}
                    item={item}
                    onPreview={openPreview}
                    scrollRoot={scrollRoot}
                  />
                ))}

                {nextOffset < total ? (
                  <Button
                    variant="outline"
                    size="sm"
                    className="w-full"
                    disabled={loadingMore}
                    onClick={() => void loadMore()}
                  >
                    {loadingMore ? (
                      <>
                        <Loader2
                          className="h-3.5 w-3.5 animate-spin"
                          aria-hidden="true"
                        />
                        加载中
                      </>
                    ) : (
                      "加载更多"
                    )}
                  </Button>
                ) : null}
              </div>
            </ScrollArea>
          </div>
        </TabsContent>
      </Tabs>

      {previewItem ? (
        <ArtifactPreviewDialog
          item={previewItem}
          triggerElRef={previewTriggerRef}
          fallbackFocusRef={panelRef}
          onClose={() => setPreviewItem(null)}
        />
      ) : null}
    </div>
  );
}
