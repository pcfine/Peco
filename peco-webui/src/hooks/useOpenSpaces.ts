// 打开集的动作层：播种、进入空间、激活空间。
//
// 「进入空间」不只是改状态：服务端要为它建（或取回）唯一 `@master` 会话。
// ensure 失败不阻塞打开 —— 左栏与导航照常可用，只是聊天页拿不到会话。

import { useCallback, useEffect } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { toast } from "sonner";
import { ensureSpaceConversation } from "@/api/spaces";
import { errorText } from "@/lib/api";
import { usePeco } from "@/queries/peco";
import { useOpenSpacesStore } from "@/stores/openSpacesStore";
import { useSpaceStore } from "@/stores/spaceStore";

/**
 * 打开集为空时用当前空间播种一次。
 *
 * 老用户的存档里只有 `currentWsId`：如果左栏从空开始，「已打开」就等于一个空间都没有，
 * 与「他正在用的那个空间是打开的」自相矛盾。Peco 有自己的行，不参与播种。
 */
export function useSeedOpenSpaces(): void {
  const { data: peco } = usePeco();
  const currentWsId = useSpaceStore((state) => state.currentWsId);
  const seeded = useOpenSpacesStore((state) => state.seeded);
  const openIds = useOpenSpacesStore((state) => state.openIds);
  const seedOnce = useOpenSpacesStore((state) => state.seedOnce);

  useEffect(() => {
    if (seeded || openIds.length > 0) return;
    if (!peco || !currentWsId || currentWsId === peco.ws_id) return;
    seedOnce(currentWsId);
  }, [peco, currentWsId, seeded, openIds.length, seedOnce]);
}

/** 从工作室进入空间：加入打开集、确保唯一会话、跳到聊天页。 */
export function useEnterSpace(): (wsId: string) => void {
  const navigate = useNavigate();
  const open = useOpenSpacesStore((state) => state.open);

  return useCallback(
    (wsId: string) => {
      open(wsId);
      void ensureSpaceConversation(wsId).catch((error: unknown) => {
        toast.error(errorText(error));
      });
      navigate(`/spaces/${encodeURIComponent(wsId)}`);
    },
    [navigate, open],
  );
}

/**
 * 左栏行主点击：已经在打开集里，只切激活态并回到聊天页。
 *
 * 不重发 ensure：重复点击不该产生第二条会话，也不该白打一次请求。
 */
export function useActivateSpace(): (wsId: string) => void {
  const navigate = useNavigate();
  const { pathname } = useLocation();
  const setActive = useOpenSpacesStore((state) => state.setActive);

  return useCallback(
    (wsId: string) => {
      setActive(wsId);
      const target = `/spaces/${encodeURIComponent(wsId)}`;
      if (pathname !== target) navigate(target);
    },
    [navigate, pathname, setActive],
  );
}
