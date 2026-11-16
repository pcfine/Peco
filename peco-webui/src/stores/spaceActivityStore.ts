// 按空间记录对话是否在跑。
//
// 同一条会话不止一个观察者：页面壳为每个打开的空间常驻（隐藏的也挂着），抽屉又是
// 一个。记成单布尔时，后挂载的那个会用自己那边的「空闲」盖掉别人正在跑的事实，
// 所以按 owner 记集合 —— 任一在跑即为在跑。

import { create } from "zustand";

interface SpaceActivityState {
  /** wsId → 正在跑的 owner 列表。 */
  running: Record<string, string[]>;
  setStreaming: (wsId: string, owner: string, streaming: boolean) => void;
}

export const useSpaceActivityStore = create<SpaceActivityState>((set) => ({
  running: {},
  setStreaming: (wsId, owner, streaming) =>
    set((state) => {
      const owners = state.running[wsId] ?? [];
      if (owners.includes(owner) === streaming) return state;
      const next = streaming
        ? [...owners, owner]
        : owners.filter((item) => item !== owner);
      const running = { ...state.running };
      // 空集合就是「没人在跑」：留着只会让记录越来越长。
      if (next.length > 0) running[wsId] = next;
      else delete running[wsId];
      return { running };
    }),
}));

/** 某个空间有任何一个观察者在跑即为真；null / 未知一律按空闲算。 */
export function useSpaceStreaming(wsId: string | null | undefined): boolean {
  return useSpaceActivityStore((state) =>
    wsId ? (state.running[wsId]?.length ?? 0) > 0 : false,
  );
}
