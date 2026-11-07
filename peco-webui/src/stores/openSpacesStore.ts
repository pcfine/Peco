// 左栏的打开集：哪些空间常驻、哪一行是激活态。
//
// 它只描述界面上的「已打开」，与 daemon 侧空间是否在 Hub 里是两件事 ——
// 从打开集移除不关闭空间。

import { create } from "zustand";
import { persist } from "zustand/middleware";
import { useSpaceStore } from "@/stores/spaceStore";

interface OpenSpacesState {
  /** 加入顺序即渲染顺序：最近打开的不跳到最前，用户记住的位置不能被重排。 */
  openIds: string[];
  activeWsId: string | null;
  /** 是否已经用 `currentWsId` 播过种：播过之后空集合是用户的真实选择，不能再填回来。 */
  seeded: boolean;
  /** 只动集合与激活态；唯一会话的创建属于「进入空间」这个动作，见 `useEnterSpace`。 */
  open: (wsId: string) => void;
  close: (wsId: string) => void;
  setActive: (wsId: string) => void;
  seedOnce: (wsId: string) => void;
}

export const useOpenSpacesStore = create<OpenSpacesState>()(
  persist(
    (set, get) => ({
      openIds: [],
      activeWsId: null,
      seeded: false,
      open: (wsId) => {
        const { openIds } = get();
        set({
          openIds: openIds.includes(wsId) ? openIds : [...openIds, wsId],
          activeWsId: wsId,
          seeded: true,
        });
        useSpaceStore.getState().setCurrent(wsId);
      },
      close: (wsId) => {
        const { openIds, activeWsId } = get();
        set({
          openIds: openIds.filter((id) => id !== wsId),
          activeWsId: activeWsId === wsId ? null : activeWsId,
        });
      },
      setActive: (wsId) => {
        set({ activeWsId: wsId });
        useSpaceStore.getState().setCurrent(wsId);
      },
      seedOnce: (wsId) => {
        if (get().seeded) return;
        set({ openIds: [wsId], activeWsId: wsId, seeded: true });
      },
    }),
    {
      name: "peco-webui:open-spaces",
      partialize: (state) => ({
        openIds: state.openIds,
        activeWsId: state.activeWsId,
        seeded: state.seeded,
      }),
    },
  ),
);

/**
 * 「激活」与请求归属（`X-Peco-Space`）必须是同一件事。
 *
 * 改当前空间的入口不止左栏（顶栏切换器、空间头部的「设为当前」、启动校正），
 * 与其要求每处都记得同步左栏，不如在这里单向镜像：少一个会漏的写点。
 */
useSpaceStore.subscribe((state, previous) => {
  if (state.currentWsId === previous.currentWsId) return;
  if (useOpenSpacesStore.getState().activeWsId === state.currentWsId) return;
  useOpenSpacesStore.setState({ activeWsId: state.currentWsId });
});
