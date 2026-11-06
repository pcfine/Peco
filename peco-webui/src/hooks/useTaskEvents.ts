// 台账事件只是「哪一行变了」的信号：收到就失效相关 query，由 query 自己重取。
// 连接状态写进壳级 store，dock 头部与台账顶部读同一份。

import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { openTaskStream } from "@/lib/events";
import { useConnectionStore } from "@/stores/connectionStore";
import { useSpaceStore } from "@/stores/spaceStore";

export function useTaskEvents(): void {
  const wsId = useSpaceStore((state) => state.currentWsId);
  const queryClient = useQueryClient();
  const setEvents = useConnectionStore((state) => state.setEvents);
  const retryToken = useConnectionStore((state) => state.retryToken);

  useEffect(() => {
    return openTaskStream(wsId, {
      onTask: () => {
        void queryClient.invalidateQueries({ queryKey: ["tasks"] });
        void queryClient.invalidateQueries({ queryKey: ["spaces"] });
        void queryClient.invalidateQueries({ queryKey: ["space-overview"] });
        void queryClient.invalidateQueries({ queryKey: ["space-tasks"] });
      },
      onState: setEvents,
    });
  }, [wsId, retryToken, setEvents, queryClient]);
}
