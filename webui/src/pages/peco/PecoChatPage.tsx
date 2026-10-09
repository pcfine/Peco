import { useEffect, useState } from "react";
import { ChatView } from "@/components/chat/ChatView";
import { getAssistantSupportsImages } from "@/api/agents";
import { pecoStreamUrl, uploadPecoImage } from "@/api/peco";
import { usePecoChatStore } from "@/stores/pecoChatStore";
import { useAuthStore } from "@/stores/authStore";
import { LoadingSpinner } from "@/components/common/LoadingSpinner";
import { Button } from "@/components/ui/button";
import { Trash2 } from "lucide-react";
import { toast } from "sonner";

export function PecoChatPage() {
  const messages = usePecoChatStore((s) => s.messages);
  const loading = usePecoChatStore((s) => s.loading);
  const loaded = usePecoChatStore((s) => s.loaded);
  const sessionKey = usePecoChatStore((s) => s.sessionKey);
  const isStreaming = usePecoChatStore((s) => s.isStreaming);
  const usage = usePecoChatStore((s) => s.usage);
  const load = usePecoChatStore((s) => s.load);
  const refresh = usePecoChatStore((s) => s.refresh);
  const clear = usePecoChatStore((s) => s.clear);
  const sendMessage = usePecoChatStore((s) => s.sendMessage);
  const abortStream = usePecoChatStore((s) => s.abortStream);
  const error = usePecoChatStore((s) => s.error);
  const clearError = usePecoChatStore((s) => s.clearError);
  const hasMore = usePecoChatStore((s) => s.hasMore);
  const loadingEarlier = usePecoChatStore((s) => s.loadingEarlier);
  const loadEarlier = usePecoChatStore((s) => s.loadEarlier);
  const token = useAuthStore((s) => s.token);
  const [supportsImages, setSupportsImages] = useState(false);

  useEffect(() => {
    load();
  }, [load]);

  // @assistant 的 provider 是否支持用户图片输入 —— 决定附件入口可用性。
  // 能力值静态、进程内 memo（见 api/agents），路由往返与 StrictMode 双跑各只探一次。
  useEffect(() => {
    let cancelled = false;
    getAssistantSupportsImages()
      .then((supported) => {
        if (!cancelled) setSupportsImages(supported);
      })
      .catch(() => {
        // 取不到能力信息时保持禁用附件入口
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // 切回标签页 / 窗口获焦时追平会话（多窗口最终一致）：
  // 重拉快照恢复已提交轮次，并检测服务端进行中的任务重新附着。
  useEffect(() => {
    const onVisible = () => {
      if (document.visibilityState === "visible") refresh();
    };
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [refresh]);

  // Surface streaming errors via toast.
  useEffect(() => {
    if (error) {
      toast.error(error);
      clearError();
    }
  }, [error, clearError]);

  const handleClear = async () => {
    try {
      await clear();
      toast.success("对话已清除");
    } catch {
      toast.error("清除失败");
    }
  };

  // 首次加载且无缓存数据时显示 loading
  if (loading && !loaded) return <LoadingSpinner />;

  return (
    <ChatView
      key={sessionKey}
      mode="external"
      streamUrl={pecoStreamUrl}
      supportsImages={supportsImages}
      uploadImage={(file) => uploadPecoImage(file)}
      initialMessages={messages}
      onExternalSend={(text, images) => {
        sendMessage(text, token ?? "", images);
      }}
      onExternalStop={abortStream}
      externalIsStreaming={isStreaming}
      externalUsage={usage}
      onLoadEarlier={loadEarlier}
      hasMore={hasMore}
      loadingEarlier={loadingEarlier}
      headerTitle="Peco"
      headerActions={
        <Button variant="ghost" size="sm" onClick={handleClear}>
          <Trash2 className="h-4 w-4 mr-1" />
          清除对话
        </Button>
      }
      welcomeMessage={
        <>
          <p className="text-lg">👋 你好！我是 Peco，你的个人 AI 助理。</p>
          <p className="text-sm mt-2">
            由 @assistant 驱动 · 我可以执行命令、管理记忆、搜索知识库。
          </p>
        </>
      }
    />
  );
}
