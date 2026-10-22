// 卡片取流：视口进出配对 retain/release，进入视口后才发起取流。

import { useCallback, useEffect, useRef, useState } from "react";
import {
  ArtifactBlobError,
  getArtifactObjectUrl,
  release,
  retain,
} from "@/api/artifactBlob";
import { usePecoArtifactStore } from "@/stores/pecoArtifactStore";
import type { ArtifactItem } from "@/types/artifact";

export type ArtifactBlobStatus = "idle" | "loading" | "ready" | "error";

export interface UseArtifactBlobOptions {
  /**
   * `false` 时既不挂视口观察器也不取流：视频/音频要等用户点开查看器才下载
   * 整份文件，进视口就 retain 会让播过的文件在卡片可见期间永不可淘汰。
   */
  enabled?: boolean;
  /** 被观察的元素（卡片里的预览区）。hook 不渲染 DOM，锚点只能由调用方给。 */
  containerRef?: React.RefObject<HTMLElement | null>;
  /**
   * 滚动可视区元素，作为观察的 root。
   *
   * 省略即按窗口判断：面板内滚动不会让卡片"离开"窗口，release 永不触发，
   * 缓存只剩条目数上限兜底。
   */
  scrollRoot?: HTMLElement | null;
}

export interface UseArtifactBlobResult {
  status: ArtifactBlobStatus;
  url?: string;
  /** 响应头里的类型，交给调用方判定这张 blob 能不能真的塞进 `<img>` 之类。 */
  mime?: string;
  retry: () => void;
}

interface BlobState {
  status: ArtifactBlobStatus;
  url?: string;
  mime?: string;
}

export function useArtifactBlob(
  item: ArtifactItem,
  options: UseArtifactBlobOptions = {},
): UseArtifactBlobResult {
  const { enabled = true, containerRef, scrollRoot = null } = options;
  const { id } = item;

  const [state, setState] = useState<BlobState>({ status: "idle" });
  const [inView, setInView] = useState(false);
  const [attempt, setAttempt] = useState(0);

  /** 上一次 retain 是否还没归还：cleanup 里据此幂等，避免把计数减成负数。 */
  const retainedRef = useRef(false);
  const itemRef = useRef(item);
  itemRef.current = item;

  useEffect(() => {
    // enabled 为假时连观察器都不该挂：不挂就不会 retain，也就不会把
    // 视频/音频的 blob 钉在缓存里
    if (!enabled) return;
    const node = containerRef?.current;
    if (!node) return;

    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          setInView(entry.isIntersecting);
          if (entry.isIntersecting) {
            if (retainedRef.current) continue;
            retainedRef.current = true;
            retain(id);
          } else {
            if (!retainedRef.current) continue;
            retainedRef.current = false;
            release(id);
          }
        }
      },
      { root: scrollRoot ?? undefined, rootMargin: "200px" },
    );
    observer.observe(node);

    return () => {
      // `disconnect()` 不产生 leave 回调：不在这里归还，条目会永远不可淘汰
      observer.disconnect();
      if (!retainedRef.current) return;
      retainedRef.current = false;
      release(id);
    };
  }, [containerRef, enabled, id, scrollRoot]);

  useEffect(() => {
    if (!inView || !enabled) return;

    let cancelled = false;
    const controller = new AbortController();
    // 已就绪的条目在离开再进入视口时不该退回占位骨架
    setState((prev) =>
      prev.status === "ready" ? prev : { status: "loading" },
    );

    getArtifactObjectUrl(itemRef.current, {
      signal: controller.signal,
      // 滚动预取：没人要时允许出队即丢弃，否则快速滚动会把整份文件逐个下完
      requestedByUser: false,
    })
      .then((blob) => {
        if (cancelled) return;
        setState({ status: "ready", url: blob.url, mime: blob.mime });
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        if (err instanceof ArtifactBlobError && err.kind === "unavailable") {
          // 文件已不存在：写回列表，另一处挂载点上的同一张卡也不必再试
          usePecoArtifactStore.getState().markUnavailable(itemRef.current.id);
        }
        setState({ status: "error" });
      });

    return () => {
      cancelled = true;
      // 只中止本调用方的等待；共享请求与缓存照旧完成，另一处挂载点不受牵连
      controller.abort();
    };
  }, [attempt, enabled, id, inView]);

  const retry = useCallback(() => setAttempt((n) => n + 1), []);

  return { status: state.status, url: state.url, mime: state.mime, retry };
}
