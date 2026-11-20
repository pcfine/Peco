// 台账事件流：`EventSource` 设不了请求头，空间只能走 `?ws=`。
//
// 断线不用浏览器内建的重连：它的间隔不可见也不可调，而界面上要写「第 n/6 次」
// 写在界面上。这里自己排退避，并把每次尝试报给调用方。

import type { TaskEvent } from "@/types/daemon";
import { getAuthStatus } from "@/api/auth";
import { MAX_RETRIES, retryDelay, type SseState } from "@/lib/sseState";
import { useAuthStore } from "@/stores/authStore";

/**
 * 事件流重连用尽后问一次会话状态。
 *
 * `EventSource` 拿不到状态码：登录过期与 daemon 掉线在它眼里是同一个 error，
 * 只有 `/auth/status` 能分辨。探测本身失败说明不了会话有问题（多半是 daemon
 * 不在），交给闸门自己的错误态，不在这里再报一次。
 */
async function probeSession(): Promise<void> {
  try {
    const status = await getAuthStatus();
    if (!status.authenticated) useAuthStore.getState().markUnauthenticated();
  } catch {
    // 探测无果：保持现状，重连状态机已经在界面上说了「连接失败」。
  }
}

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
  /** 每次掉线只探一次会话：失败态下不会再有 error，但可见性恢复会重连一次。 */
  let probed = false;

  const detach = () => {
    source?.close();
    source = null;
  };

  const schedule = () => {
    if (attempt >= MAX_RETRIES) {
      onState({ kind: "failed" });
      if (!probed) {
        probed = true;
        void probeSession();
      }
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
      probed = false;
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
