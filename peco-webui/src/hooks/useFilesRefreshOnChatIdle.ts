// 聊天一轮结束时失效文件树：agent 常常一边回话一边落文件，等轮询最多慢 7 秒。
//
// 只在「生成中 → 非生成中」这一下失效。每次渲染都失效会让文件面板一直处在重取中，
// 用户看到的是闪个不停而永远填不满的列表。

import { useEffect, useRef } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useConnectionStore } from "@/stores/connectionStore";

export function useFilesRefreshOnChatIdle(): void {
  const chatKind = useConnectionStore((state) => state.chat.kind);
  const queryClient = useQueryClient();
  const wasRunning = useRef(chatKind === "running");

  useEffect(() => {
    const running = chatKind === "running";
    if (wasRunning.current && !running) {
      void queryClient.invalidateQueries({ queryKey: ["space-files"] });
    }
    wasRunning.current = running;
  }, [chatKind, queryClient]);
}
