// 当前空间是壳级全局态：每个请求头（`X-Peco-Space`）与 SSE 的 `?ws=` 都读它。
// 只存 id，不存对象 —— 空间详情随列表轮询变化，存对象就会有一份会过期的副本。

import { create } from "zustand";
import { persist } from "zustand/middleware";
import type { PecoIdentity } from "@/types/daemon";

interface SpaceState {
  /** 用户选中的空间；`null` = 尚未决定（首次进入，等 `/api/peco` 回话）。 */
  currentWsId: string | null;
  /** Peco 总管身份：解析出的 ws_id 可能与种子常量不同，只信这个接口。 */
  peco: PecoIdentity | null;
  setCurrent: (wsId: string) => void;
  setPeco: (peco: PecoIdentity) => void;
}

export const useSpaceStore = create<SpaceState>()(
  persist(
    (set) => ({
      currentWsId: null,
      peco: null,
      setCurrent: (wsId) => set({ currentWsId: wsId }),
      setPeco: (peco) => set({ peco }),
    }),
    {
      name: "peco-webui:ws",
      // peco 身份每次启动都能重取，落盘反而会让改过记录文件的用户拿到旧 ws_id。
      partialize: (state) => ({ currentWsId: state.currentWsId }),
    },
  ),
);
