// 产物卡片：预览区 + 两个并列按钮（预览 / 下载）。

import { useRef } from "react";
import type { ReactNode } from "react";
import {
  Download,
  File,
  FileText,
  FileX,
  Film,
  ImageOff,
  Music,
  Play,
} from "lucide-react";
import { toast } from "sonner";
import {
  ARTIFACT_BLOB_MAX_BYTES,
  ARTIFACT_INLINE_BLOB_MAX_BYTES,
  classifyPreview,
  downloadArtifact,
} from "@/api/artifactBlob";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";
import { formatBytes, formatCreatedAt } from "@/lib/format";
import type { ArtifactItem } from "@/types/artifact";
import { useArtifactBlob } from "./useArtifactBlob";

const KIND_LABELS: Record<string, string> = {
  image: "图片",
  video: "视频",
  audio: "音频",
  document: "文档",
};

const PREVIEWABLE_KINDS = new Set(["image", "video", "audio"]);

export interface ArtifactCardProps {
  item: ArtifactItem;
  /** 一并给出触发按钮：查看器关闭时焦点要还回用户按下的那个元素。 */
  onPreview: (item: ArtifactItem, trigger: HTMLButtonElement) => void;
  /** 面板滚动容器的可视区元素，交给视口观察当 root。 */
  scrollRoot?: HTMLElement | null;
}

/** 文档没有内联预览，扩展名是用户能看到的唯一"这是什么文件"线索。 */
function extensionLabel(item: ArtifactItem): string {
  const fromTitle = /\.([a-z0-9]{1,8})$/i.exec(item.title);
  if (fromTitle) return fromTitle[1].toUpperCase();
  const subtype = item.mime
    .split(";")[0]
    .split("/")[1]
    ?.replace(/^x-/, "")
    .trim();
  return subtype && subtype.length <= 8 ? subtype.toUpperCase() : "文件";
}

function Placeholder({
  icon,
  caption,
  badge,
}: {
  icon: ReactNode;
  caption?: string;
  badge?: ReactNode;
}) {
  return (
    <div className="flex h-32 w-full flex-col items-center justify-center gap-1 bg-muted/40 text-muted-foreground">
      <span className="relative flex items-center justify-center">
        {icon}
        {badge}
      </span>
      {caption ? <span className="text-[11px]">{caption}</span> : null}
    </div>
  );
}

export function ArtifactCard({
  item,
  onPreview,
  scrollRoot = null,
}: ArtifactCardProps) {
  const previewRef = useRef<HTMLDivElement>(null);

  const expired = item.available === false;
  const overHardLimit = item.size_bytes > ARTIFACT_BLOB_MAX_BYTES;
  const overInlineLimit =
    (item.kind === "video" || item.kind === "audio") &&
    item.size_bytes > ARTIFACT_INLINE_BLOB_MAX_BYTES;
  const isImage = item.kind === "image";
  const canPreview = PREVIEWABLE_KINDS.has(item.kind);

  const { status, url, mime, retry } = useArtifactBlob(item, {
    // 只有图片进视口即取流；视频/音频要等用户点开查看器，否则滚过去就静默下载整份文件
    enabled: isImage && !expired && !overHardLimit,
    // 视频/音频不交给视口观察：进了视口就 retain，播过的 blob 在卡片可见期间
    // 永不可淘汰（缓存上界失守）
    containerRef: isImage ? previewRef : undefined,
    scrollRoot,
  });

  /**
   * 响应类型说了算：服务端把白名单外的类型降级成 `application/octet-stream`，
   * 不校验就把 blob 塞进 `<img>` 是静默裂图，用户看不到任何原因。
   */
  const imagePreviewable =
    isImage &&
    status === "ready" &&
    url !== undefined &&
    classifyPreview(mime ?? "", item.kind) === "image";
  const imageMimeMismatch = isImage && status === "ready" && !imagePreviewable;

  const downloadDisabled = expired || overHardLimit;
  const previewDisabled =
    expired ||
    overHardLimit ||
    overInlineLimit ||
    (isImage && status === "error") ||
    imageMimeMismatch;

  const sizeMb = Math.round(item.size_bytes / (1024 * 1024));
  const overHardLimitHint =
    item.kind === "video" || item.kind === "audio"
      ? "文件超过 200 MB，暂不支持浏览器预览或下载"
      : "文件超过 200 MB，暂不支持浏览器下载";
  const limitHint = overHardLimit
    ? overHardLimitHint
    : overInlineLimit
      ? `文件较大（${sizeMb} MB），请下载后播放`
      : null;

  const handleDownload = async () => {
    try {
      await downloadArtifact(item);
    } catch {
      toast.error("下载失败");
    }
  };

  const renderPreview = () => {
    if (expired) {
      return (
        <Placeholder
          icon={
            isImage ? (
              <ImageOff className="h-6 w-6" aria-hidden="true" />
            ) : (
              <FileX className="h-6 w-6" aria-hidden="true" />
            )
          }
          caption="文件已过期"
        />
      );
    }
    if (isImage) {
      // 超过硬上限根本不发取流，状态停在 idle：继续渲染骨架就是"永远在加载"
      if (overHardLimit) {
        return (
          <Placeholder
            icon={<ImageOff className="h-6 w-6" aria-hidden="true" />}
            caption="文件过大"
          />
        );
      }
      if (status === "error" || imageMimeMismatch) {
        return (
          <Placeholder
            icon={<ImageOff className="h-6 w-6" aria-hidden="true" />}
            caption="无法预览"
          />
        );
      }
      if (status === "ready" && url && imagePreviewable) {
        return (
          <img
            src={url}
            alt={item.title || "图片产物"}
            className="h-32 w-full object-cover"
          />
        );
      }
      return <Skeleton className="h-32 w-full" />;
    }
    if (item.kind === "video") {
      return (
        <Placeholder
          icon={<Film className="h-6 w-6" aria-hidden="true" />}
          badge={
            <Play
              className="absolute h-6 w-6 fill-current text-foreground/70"
              aria-hidden="true"
            />
          }
        />
      );
    }
    if (item.kind === "audio") {
      return (
        <Placeholder icon={<Music className="h-6 w-6" aria-hidden="true" />} />
      );
    }
    if (item.kind === "document") {
      return (
        <Placeholder
          icon={<FileText className="h-6 w-6" aria-hidden="true" />}
          caption={extensionLabel(item)}
        />
      );
    }
    // 后端新增类别时不能渲染出空白：图标兜底 + 原始 kind 文本
    return (
      <Placeholder
        icon={<File className="h-6 w-6" aria-hidden="true" />}
        caption={String(item.kind)}
      />
    );
  };

  return (
    <div className="rounded-lg border bg-card p-2.5">
      <div ref={previewRef} className="relative overflow-hidden rounded-md">
        {renderPreview()}
        {canPreview ? (
          <button
            type="button"
            aria-label={`预览 ${item.title}`}
            disabled={previewDisabled}
            aria-disabled={previewDisabled}
            onClick={(event) => onPreview(item, event.currentTarget)}
            className="absolute inset-0 cursor-pointer rounded-md focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:cursor-not-allowed"
          />
        ) : null}
        <button
          type="button"
          aria-label={`下载 ${item.title}`}
          disabled={downloadDisabled}
          aria-disabled={downloadDisabled}
          onClick={() => void handleDownload()}
          className="absolute top-1.5 right-1.5 z-10 cursor-pointer rounded-md bg-background/90 p-1.5 text-foreground shadow-xs transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-50"
        >
          <Download className="h-3.5 w-3.5" aria-hidden="true" />
        </button>
      </div>

      <p className="mt-2 line-clamp-2 text-xs font-medium" title={item.title}>
        {item.title}
      </p>

      <div className="mt-1.5 flex items-center gap-1.5 text-[11px] text-muted-foreground">
        <Badge variant="outline" className="text-[10px]">
          {KIND_LABELS[item.kind] ?? String(item.kind)}
        </Badge>
        <span>{formatBytes(item.size_bytes)}</span>
        <span className="truncate">{formatCreatedAt(item.created_at)}</span>
      </div>

      {limitHint ? (
        <p className="mt-1 text-[11px] text-muted-foreground">{limitHint}</p>
      ) : null}

      {isImage && status === "error" ? (
        <button
          type="button"
          onClick={retry}
          className="mt-1 cursor-pointer text-[11px] text-primary underline underline-offset-2"
        >
          重试
        </button>
      ) : null}
    </div>
  );
}
