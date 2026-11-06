// 当前空间的校正：首次进入指向 Peco；存档里的空间已被删则回落并告知。

import { useEffect } from "react";
import { toast } from "sonner";
import { usePeco } from "@/queries/peco";
import { useSpaces } from "@/queries/spaces";
import { useSpaceStore } from "@/stores/spaceStore";

export function useResolveCurrentSpace(): void {
  const { data: peco } = usePeco();
  const { data: spaces } = useSpaces();
  const currentWsId = useSpaceStore((state) => state.currentWsId);
  const setCurrent = useSpaceStore((state) => state.setCurrent);

  useEffect(() => {
    if (!peco || !spaces) return;
    if (!currentWsId) {
      setCurrent(peco.ws_id);
      return;
    }
    if (spaces.some((space) => space.ws_id === currentWsId)) return;
    setCurrent(peco.ws_id);
    toast("原空间已不存在，已切回 Peco");
  }, [peco, spaces, currentWsId, setCurrent]);
}
