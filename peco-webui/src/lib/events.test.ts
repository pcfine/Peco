import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { openTaskStream } from "@/lib/events";
import { MAX_RETRIES, retryDelay } from "@/lib/sseState";
import { useAuthStore } from "@/stores/authStore";
import { getAuthStatus } from "@/api/auth";

// `EventSource` 不给状态码：登录过期与 daemon 掉线都是同一个 error。只有重连
// 用尽之后问一次会话状态才知道该不该把人送回登录页 —— 每次 error 都探一遍
// 会在 daemon 掉线时打满 /auth/status。

vi.mock("@/api/auth", () => ({ getAuthStatus: vi.fn() }));

class FakeEventSource {
  static instances: FakeEventSource[] = [];
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  closed = false;

  constructor(readonly url: string) {
    FakeEventSource.instances.push(this);
  }

  addEventListener() {}

  close() {
    this.closed = true;
  }

  emitOpen() {
    this.onopen?.();
  }

  emitError() {
    this.onerror?.();
  }
}

/** 当前那条连接。 */
function live(): FakeEventSource {
  const source = FakeEventSource.instances.at(-1);
  if (!source) throw new Error("没有建立连接");
  return source;
}

/** 把重连次数耗光：每次 error 之后推进到下一次连接。 */
async function exhaustRetries() {
  for (let attempt = 1; attempt <= MAX_RETRIES; attempt += 1) {
    live().emitError();
    await vi.advanceTimersByTimeAsync(retryDelay(attempt));
  }
  // 最后一次 error 时 attempt 已到上限：直接进 failed 并触发探测。
  live().emitError();
  await vi.advanceTimersByTimeAsync(0);
}

beforeEach(() => {
  FakeEventSource.instances = [];
  vi.stubGlobal(
    "EventSource",
    FakeEventSource as unknown as typeof EventSource,
  );
  vi.useFakeTimers();
  vi.mocked(getAuthStatus).mockReset();
  useAuthStore.setState({
    registered: true,
    authenticated: true,
    username: "pcfine",
    checking: false,
  });
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("事件流断线后的会话探测", () => {
  it("重连未用尽前不探测", async () => {
    const stop = openTaskStream(null, { onTask: () => {}, onState: () => {} });

    live().emitError();
    await vi.advanceTimersByTimeAsync(retryDelay(1));

    expect(getAuthStatus).not.toHaveBeenCalled();
    stop();
  });

  it("重连用尽后探一次：会话已失效就翻掉会话态", async () => {
    vi.mocked(getAuthStatus).mockResolvedValue({
      registered: true,
      authenticated: false,
      username: null,
    });
    const states: string[] = [];
    const stop = openTaskStream(null, {
      onTask: () => {},
      onState: (state) => states.push(state.kind),
    });

    await exhaustRetries();

    expect(states).toContain("failed");
    expect(getAuthStatus).toHaveBeenCalledTimes(1);
    expect(useAuthStore.getState().authenticated).toBe(false);
    stop();
  });

  it("会话仍有效时什么都不做：探测只决定要不要送回登录页", async () => {
    vi.mocked(getAuthStatus).mockResolvedValue({
      registered: true,
      authenticated: true,
      username: "pcfine",
    });
    const stop = openTaskStream(null, { onTask: () => {}, onState: () => {} });

    await exhaustRetries();

    expect(getAuthStatus).toHaveBeenCalledTimes(1);
    expect(useAuthStore.getState().authenticated).toBe(true);
    stop();
  });

  it("失败态里再来的 error 不重复探测", async () => {
    vi.mocked(getAuthStatus).mockResolvedValue({
      registered: true,
      authenticated: false,
      username: null,
    });
    const stop = openTaskStream(null, { onTask: () => {}, onState: () => {} });

    await exhaustRetries();
    live().emitError();
    await vi.advanceTimersByTimeAsync(0);

    expect(getAuthStatus).toHaveBeenCalledTimes(1);
    stop();
  });

  it("探测本身失败时不误判成未登录", async () => {
    vi.mocked(getAuthStatus).mockRejectedValue(new Error("连不上 daemon"));
    const stop = openTaskStream(null, { onTask: () => {}, onState: () => {} });

    await exhaustRetries();

    expect(getAuthStatus).toHaveBeenCalledTimes(1);
    expect(useAuthStore.getState().authenticated).toBe(true);
    stop();
  });
});
