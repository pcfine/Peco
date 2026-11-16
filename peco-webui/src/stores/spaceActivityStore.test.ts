import { beforeEach, describe, expect, it } from "vitest";
import { act, renderHook } from "@testing-library/react";
import {
  useSpaceActivityStore,
  useSpaceStreaming,
} from "@/stores/spaceActivityStore";

const setStreaming = (wsId: string, owner: string, streaming: boolean) =>
  act(() => {
    useSpaceActivityStore.getState().setStreaming(wsId, owner, streaming);
  });

const ownersOf = (wsId: string) =>
  useSpaceActivityStore.getState().running[wsId];

beforeEach(() => {
  useSpaceActivityStore.setState({ running: {} });
});

describe("空间对话活动", () => {
  it("后挂载的观察者上报空闲，不会盖掉还在跑的另一个", () => {
    const { result } = renderHook(() => useSpaceStreaming("ws_peco"));
    expect(result.current).toBe(false);

    setStreaming("ws_peco", "dock", true);
    expect(result.current).toBe(true);

    // 隐藏的页面壳挂载即上报「我这边空闲」：单布尔会在这里把抽屉的 true 冲掉。
    setStreaming("ws_peco", "space-shell", false);
    expect(result.current).toBe(true);
    expect(ownersOf("ws_peco")).toEqual(["dock"]);
  });

  it("卸载一个观察者只撤自己那一笔，全部撤完才回空闲", () => {
    const { result } = renderHook(() => useSpaceStreaming("ws_peco"));
    setStreaming("ws_peco", "space-shell", true);
    setStreaming("ws_peco", "dock", true);

    setStreaming("ws_peco", "dock", false);
    expect(result.current).toBe(true);

    setStreaming("ws_peco", "space-shell", false);
    expect(result.current).toBe(false);
    // 空集合不留残影：否则「谁在跑」的列表会越攒越长。
    expect(ownersOf("ws_peco")).toBeUndefined();
  });

  it("按空间分开记，重复上报同一状态不产生第二项", () => {
    const { result } = renderHook(() => useSpaceStreaming("ws_a"));
    setStreaming("ws_a", "dock", true);
    setStreaming("ws_a", "dock", true);
    setStreaming("ws_b", "dock", true);

    expect(ownersOf("ws_a")).toEqual(["dock"]);
    expect(result.current).toBe(true);

    setStreaming("ws_a", "dock", false);
    expect(result.current).toBe(false);
    // 别的空间还在跑，不跟着被清。
    expect(ownersOf("ws_b")).toEqual(["dock"]);
  });
});
