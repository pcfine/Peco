// 台账事件流：`EventSource` 设不了请求头，空间只能走 `?ws=`。
//
// 断线不用浏览器内建的重连：它的间隔不可见也不可调，而界面上要写「第 n/6 次」
// 写在界面上。这里自己排退避，并把每次尝试报给调用方。

import type { TaskEvent } from "@/types/daemon";
import { MAX_RETRIES, retryDelay, type SseState } from "@/lib/sseState";

export interface TaskStreamHandlers {
  /** 事件本身是「哪一行变了」的通知，不是行内容 —— 回调里只该失效或重取 query。 */
  onTask: (event: TaskEvent) => void;
  onState: (state: SseState) => void;
}

export function openTaskStream(
  wsId: string | null,
  { onTask, onState }: TaskStreamHandlers,
): () => void {
  const url = `/api/events${wsId ? `?ws=${encodeURIComponent(wsId)}` : ""}`;
  let source: EventSource | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let attempt = 0;
  let stopped = false;

  const detach = () => {
    source?.close();
    source = null;
  };

  const schedule = () => {
    if (attempt >= MAX_RETRIES) {
      onState({ kind: "failed" });
      return;
    }
    attempt += 1;
    onState({ kind: "reconnecting", attempt });
    timer = setTimeout(connect, retryDelay(attempt));
  };

  const connect = () => {
    if (stopped) return;
    onState(
      attempt === 0 ? { kind: "idle" } : { kind: "reconnecting", attempt },
    );
    source = new EventSource(url);
    source.onopen = () => {
      attempt = 0;
      onState({ kind: "open" });
    };
    source.addEventListener("task", ((raw: MessageEvent<string>) => {
      try {
        onTask(JSON.parse(raw.data) as TaskEvent);
      } catch {
        // 载荷解析失败不该让订阅挂掉：下一次事件照样能处理。
      }
    }) as EventListener);
    source.onerror = () => {
      detach();
      if (stopped) return;
      // 页面不可见时不排重试：后台空转不如回到可见时补一次。
      if (typeof document !== "undefined" && document.hidden) return;
      schedule();
    };
  };

  const onVisible = () => {
    if (stopped || source || typeof document === "undefined") return;
    if (!document.hidden) {
      clearTimeout(timer);
      connect();
    }
  };

  document.addEventListener("visibilitychange", onVisible);
  connect();

  return () => {
    stopped = true;
    clearTimeout(timer);
    document.removeEventListener("visibilitychange", onVisible);
    detach();
  };
}
