// 按空间记录对话是否在跑。
//
// 页面里挂着的隐藏聊天壳同样会上报；活动不是「当前页」的属性，
// connectionStore.chat 那份全局单值区分不出是谁在跑。

import { create } from "zustand";

interface SpaceActivityState {
  streaming: Record<string, boolean>;
  setStreaming: (wsId: string, streaming: boolean) => void;
}

export const useSpaceActivityStore = create<SpaceActivityState>((set) => ({
  streaming: {},
  setStreaming: (wsId, streaming) =>
    set((state) =>
      state.streaming[wsId] === streaming
        ? state
        : { streaming: { ...state.streaming, [wsId]: streaming } },
    ),
}));

/** 某个空间的对话是否在跑；null / 未知一律按空闲算。 */
export function useSpaceStreaming(wsId: string | null | undefined): boolean {
  return useSpaceActivityStore((state) =>
    wsId ? (state.streaming[wsId] ?? false) : false,
  );
}
