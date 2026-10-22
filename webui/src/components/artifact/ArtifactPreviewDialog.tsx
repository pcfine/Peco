// 产物查看器：打开时才取流（图片通常命中卡片已下好的缓存）。

import { useEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import { Download } from "lucide-react";
import { toast } from "sonner";
import {
  ArtifactBlobError,
  classifyPreview,
  downloadArtifact,
  getArtifactObjectUrl,
  release,
  retain,
} from "@/api/artifactBlob";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { formatBytes, formatCreatedAt } from "@/lib/format";
import { usePecoArtifactStore } from "@/stores/pecoArtifactStore";
import type { ArtifactItem } from "@/types/artifact";

/**
 * 有内联播放器的类别。文档与未知 kind 返回 `null`：它们没有"预期媒体类型"，
 * 落进图片分支只会让判据读起来像"文档也能当图片预览"。
 */
function expectedMediaOf(kind: string): "image" | "video" | "audio" | null {
  if (kind === "image") return "image";
  if (kind === "video") return "video";
  if (kind === "audio") return "audio";
  return null;
}

export interface ArtifactPreviewDialogProps {
  item: ArtifactItem;
  /** 打开本查看器的卡片按钮。 */
  triggerElRef: RefObject<HTMLButtonElement | null>;
  /** 触发按钮已不可聚焦时的落点：面板容器。 */
  fallbackFocusRef: RefObject<HTMLElement | null>;
  onClose: () => void;
}

interface PreviewState {
  status: "loading" | "ready" | "error";
  url?: string;
  mime?: string;
}

export function ArtifactPreviewDialog({
  item,
  triggerElRef,
  fallbackFocusRef,
  onClose,
}: ArtifactPreviewDialogProps) {
  const [state, setState] = useState<PreviewState>({ status: "loading" });
  const [progress, setProgress] = useState(0);
  const [attempt, setAttempt] = useState(0);

  const itemRef = useRef(item);
  itemRef.current = item;
  /** 上一次播报过的进度桶，保证按 10% 步进推进而不是每个 chunk 都重渲染。 */
  const reportedRef = useRef(0);

  useEffect(() => {
    retain(item.id);
    // 关闭只让它"可淘汰"，不 revoke：同一份字节可能正被卡片缩略图使用
    return () => release(item.id);
  }, [item.id]);

  useEffect(() => {
    let cancelled = false;
    const controller = new AbortController();
    reportedRef.current = 0;
    setProgress(0);
    setState({ status: "loading" });

    getArtifactObjectUrl(itemRef.current, {
      requestedByUser: true,
      signal: controller.signal,
      onProgress: (percent) => {
        const bucket = percent >= 100 ? 100 : Math.floor(percent / 10) * 10;
        if (bucket <= reportedRef.current) return;
        reportedRef.current = bucket;
        setProgress(bucket);
      },
    })
      .then((blob) => {
        if (cancelled) return;
        setState({ status: "ready", url: blob.url, mime: blob.mime });
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        if (err instanceof ArtifactBlobError && err.kind === "unavailable") {
          // 视频/音频只在这里取流：不写回列表，卡片上的预览与下载就永远不会置灰
          usePecoArtifactStore.getState().markUnavailable(itemRef.current.id);
        }
        setState({ status: "error" });
      });

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, [attempt, item.id]);

  const handleDownload = async () => {
    try {
      await downloadArtifact(item);
    } catch {
      toast.error("下载失败");
    }
  };

  /**
   * 只有回应了预期媒体类型才允许进播放器/`<img>`：服务端把白名单外降级成
   * `application/octet-stream`，硬塞进去只会得到一块黑屏或裂图。
   */
  const expectedMedia = expectedMediaOf(item.kind);
  const playable =
    expectedMedia !== null &&
    state.status === "ready" &&
    state.url !== undefined &&
    classifyPreview(state.mime ?? "", item.kind) === expectedMedia;

  const metaLine = [
    item.title || "产物预览",
    formatBytes(item.size_bytes),
    formatCreatedAt(item.created_at),
  ]
    .filter((part) => part.length > 0)
    .join(" · ");

  const renderMedia = (url: string) => {
    if (item.kind === "image") {
      return (
        <img
          src={url}
          alt={item.title || "图片产物"}
          className="max-h-[80vh] w-auto object-contain"
        />
      );
    }
    if (item.kind === "video") {
      return (
        <video
          src={url}
          controls
          playsInline
          autoPlay
          className="max-h-[80vh] w-full"
        />
      );
    }
    return <audio src={url} controls autoPlay className="w-full" />;
  };

  return (
    <Dialog
      open
      onOpenChange={(next) => {
        if (!next) onClose();
      }}
    >
      <DialogContent
        className="max-w-3xl"
        aria-describedby={undefined}
        onCloseAutoFocus={(event) => {
          // 查看器不是 Radix Trigger 打开的，Radix 手里没有 triggerRef、默认归还无处可去
          event.preventDefault();
          // 这个 ref 是事件里手工赋的值，React 不知道它，卸载后仍握着脱离文档的元素
          // —— `isConnected` 不是冗余
          const trigger = triggerElRef.current;
          // 打开期间列表可能被刷新，卡片连同按钮一起卸载了：无处可还
          if (!trigger?.isConnected) return;
          // 视频/音频只在取流失败时才得知文件已过期，这一步会把卡片按钮置灰；
          // disabled 元素的 focus() 是空操作，焦点会掉到 body
          if (!trigger.disabled) {
            trigger.focus();
            return;
          }
          fallbackFocusRef.current?.focus();
        }}
      >
        <DialogHeader>
          <DialogTitle className="truncate pr-6">
            {item.title || "产物预览"}
          </DialogTitle>
        </DialogHeader>

        <div className="flex min-h-[10rem] items-center justify-center">
          {state.status === "loading" ? (
            <div className="w-full space-y-2">
              <Skeleton className="h-48 w-full" />
              <p
                role="status"
                aria-live="polite"
                className="text-center text-xs text-muted-foreground"
              >
                {progress > 0 ? `加载中 ${progress}%` : "加载中…"}
              </p>
            </div>
          ) : null}

          {state.status === "error" ? (
            <div className="space-y-2 text-center">
              <p className="text-sm text-muted-foreground">无法加载该产物</p>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setAttempt((n) => n + 1)}
              >
                重试
              </Button>
            </div>
          ) : null}

          {state.status === "ready" && !playable ? (
            <p role="status" className="text-sm text-muted-foreground">
              无法预览
            </p>
          ) : null}

          {playable && state.url ? renderMedia(state.url) : null}
        </div>

        <div className="mt-3 flex items-center justify-between gap-3 text-xs text-muted-foreground">
          <p className="truncate" title={metaLine}>
            {metaLine}
          </p>
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label={`下载 ${item.title}`}
            onClick={() => void handleDownload()}
          >
            <Download aria-hidden="true" />
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
