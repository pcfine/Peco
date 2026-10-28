import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import { ChatView } from "@/components/chat/ChatView";
import { getAssistantSupportsImages } from "@/api/agents";
import { pecoStreamUrl, uploadPecoFile, uploadPecoImage } from "@/api/peco";
import { usePecoChatStore } from "@/stores/pecoChatStore";
import { useAuthStore } from "@/stores/authStore";
import { LoadingSpinner } from "@/components/common/LoadingSpinner";
import { Button } from "@/components/ui/button";
import {
  Sheet,
  SheetClose,
  SheetContent,
  SheetTitle,
} from "@/components/ui/sheet";
import { ArtifactPanel } from "@/components/artifact/ArtifactPanel";
import { usePecoArtifactStore } from "@/stores/pecoArtifactStore";
import { useMediaQuery } from "@/hooks/useMediaQuery";
import { cn } from "@/lib/utils";
import { Package, Trash2, XIcon } from "lucide-react";
import { toast } from "sonner";

/**
 * 自绘关闭按钮：共享 sheet 那颗命中区过小且没有手型光标，用户看不出这里能点。
 *
 * 两种形态共用同一串 class：宽屏那颗能盖在面板右上角，靠的是 `<aside>` 上的
 * `relative` —— 去掉它，最近的定位祖先会退到内容区容器，按钮飞到离面板 480px 的
 * 地方。窄屏无此依赖，`SheetContent` 自己 `fixed`。
 */
const CLOSE_BTN_CLASS = cn(
  "absolute top-2 right-2 grid size-8 cursor-pointer place-items-center rounded-md",
  "text-foreground/80 transition-colors",
  "hover:bg-destructive/10 hover:text-destructive",
  "active:text-destructive",
  "ring-offset-background focus:ring-2 focus:ring-ring focus:ring-offset-2 focus:outline-hidden",
);

export function PecoChatPage() {
  const messages = usePecoChatStore((s) => s.messages);
  const loading = usePecoChatStore((s) => s.loading);
  const loaded = usePecoChatStore((s) => s.loaded);
  const sessionKey = usePecoChatStore((s) => s.sessionKey);
  const isStreaming = usePecoChatStore((s) => s.isStreaming);
  const paused = usePecoChatStore((s) => s.paused);
  const queued = usePecoChatStore((s) => s.queued);
  const usage = usePecoChatStore((s) => s.usage);
  const load = usePecoChatStore((s) => s.load);
  const refresh = usePecoChatStore((s) => s.refresh);
  const clear = usePecoChatStore((s) => s.clear);
  const sendMessage = usePecoChatStore((s) => s.sendMessage);
  const abortStream = usePecoChatStore((s) => s.abortStream);
  const pause = usePecoChatStore((s) => s.pause);
  const resume = usePecoChatStore((s) => s.resume);
  const error = usePecoChatStore((s) => s.error);
  const clearError = usePecoChatStore((s) => s.clearError);
  const hasMore = usePecoChatStore((s) => s.hasMore);
  const loadingEarlier = usePecoChatStore((s) => s.loadingEarlier);
  const loadEarlier = usePecoChatStore((s) => s.loadEarlier);
  const token = useAuthStore((s) => s.token);
  const artifactTotal = usePecoArtifactStore((s) => s.total);
  const loadArtifacts = usePecoArtifactStore((s) => s.load);
  const [supportsImages, setSupportsImages] = useState(false);
  // 宽窄共用同一个开关：跨断点时只换形态，不关闭面板，筛选（store）自然保留
  const [artifactOpen, setArtifactOpen] = useState(false);
  const isWide = useMediaQuery("(min-width: 1440px)");
  /** 面板的开关按钮：面板不是 Radix Trigger 打开的，关闭时靠它把焦点还回去。 */
  const artifactTriggerRef = useRef<HTMLButtonElement>(null);
  /** 宽屏并排容器：打开后接住焦点，Esc 只认它 DOM 内的按键。 */
  const artifactAsideRef = useRef<HTMLElement>(null);

  useEffect(() => {
    load();
  }, [load]);

  // 任何视口都只在打开时挂载，所以打开前不能只由面板发起产物请求：`load()` 在
  // `loaded` 时自己短路，所以每次进页面无条件调一次是安全的
  useEffect(() => {
    void loadArtifacts();
  }, [loadArtifacts]);

  // @assistant 的 provider 是否支持用户图片输入 —— 只决定图片文件能否发送
  // （文档不依赖视觉能力，附件入口始终可用）。能力值静态、进程内 memo
  // （见 api/agents），路由往返与 StrictMode 双跑各只探一次。
  useEffect(() => {
    let cancelled = false;
    getAssistantSupportsImages()
      .then((supported) => {
        if (!cancelled) setSupportsImages(supported);
      })
      .catch(() => {
        // 取不到能力信息时按不支持图片处理（文档附件不受影响）
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // 切回标签页 / 窗口获焦时追平会话（多窗口最终一致）：
  // 重拉快照恢复已提交轮次，并检测服务端进行中的任务重新附着。
  useEffect(() => {
    const catchUp = () => {
      refresh();
      // 产物列表与消息走同一次获焦追平（多标签 / 多设备的最终一致性）
      void usePecoArtifactStore.getState().refreshHead();
    };
    const onVisible = () => {
      if (document.visibilityState === "visible") catchUp();
    };
    window.addEventListener("focus", catchUp);
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      window.removeEventListener("focus", catchUp);
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

  // 宽屏打开后把焦点交给面板：用户点开关按钮进来时 activeElement 是按钮本身
  // （既非 body 也未脱离文档），只判 body 会让面板从未获得焦点、Esc 形同不存在。
  // 跨断点、重开两条路径也由这里兜住。窄屏一律交给 Radix 的初始聚焦（定时器驱动，
  // 父级在这里抢焦会与之打架）。
  useEffect(() => {
    if (!isWide || !artifactOpen) return;
    if (artifactAsideRef.current?.contains(document.activeElement)) return;
    artifactAsideRef.current?.focus();
  }, [isWide, artifactOpen]);

  const closeArtifacts = () => {
    setArtifactOpen(false);
    artifactTriggerRef.current?.focus();
  };

  // 预览弹窗 DOM 挂在 body 上、React 树仍在 `ArtifactPanel` 里，它的 Esc 会冒泡到
  // 这里；按 DOM 归属判定，才分得清"关面板"还是"只关预览"（窄屏由 Radix 的层栈兜住）。
  const onAsideKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.key !== "Escape") return;
    if (!artifactAsideRef.current?.contains(event.target as Node)) return;
    closeArtifacts();
  };

  const handleClear = async () => {
    try {
      await clear();
      toast.success("对话已清除");
    } catch {
      toast.error("清除失败");
    }
  };

  // 宽屏打开的是同级地标、不是弹层：`aria-haspopup="true"` 等价 menu，读屏会播报
  // 「有弹出菜单」，比不设更糟。两形态都靠 `aria-expanded` + `aria-controls` 表达
  // 关系，且 `aria-controls` 必须指向该形态下真实存在的容器 id（否则是悬空 IDREF）。
  const headerActions = (
    <div className="flex items-center gap-1">
      <Button
        ref={artifactTriggerRef}
        variant="ghost"
        size="icon-sm"
        className="relative"
        aria-label={
          artifactTotal > 0
            ? `打开产物面板，共 ${artifactTotal} 项`
            : "打开产物面板"
        }
        aria-expanded={artifactOpen}
        aria-haspopup={isWide ? undefined : "dialog"}
        aria-controls={
          artifactOpen
            ? isWide
              ? "artifact-panel-aside"
              : "artifact-panel-sheet"
            : undefined
        }
        onClick={() => setArtifactOpen(true)}
      >
        <Package className="h-4 w-4" aria-hidden="true" />
        {artifactTotal > 0 ? (
          <span
            aria-hidden="true"
            className="absolute -top-1 -right-1 rounded-full bg-primary px-1 text-[10px] leading-4 text-primary-foreground"
          >
            {artifactTotal}
          </span>
        ) : null}
      </Button>
      <Button variant="ghost" size="sm" onClick={handleClear}>
        <Trash2 className="h-4 w-4 mr-1" />
        清除对话
      </Button>
    </div>
  );

  // 首次加载且无缓存数据时显示 loading
  if (loading && !loaded) return <LoadingSpinner />;

  return (
    <div className="mx-auto flex h-[calc(100vh-7rem)] w-full gap-4">
      {/* 不进 isWide 分支：任何结构差异都会让 ChatView 卸载重建，草稿与滚动位置
          随之清空 —— 关掉面板后对话页就回不到原样了 */}
      <ChatView
        key={sessionKey}
        className="h-[calc(100vh-7rem)] flex-1 min-w-0"
        mode="external"
        streamUrl={pecoStreamUrl}
        supportsImages={supportsImages}
        uploadImage={(file, onProgress) => uploadPecoImage(file, onProgress)}
        uploadFile={(file, onProgress) => uploadPecoFile(file, onProgress)}
        initialMessages={messages}
        onExternalSend={(text, attachments) => {
          sendMessage(text, token ?? "", attachments);
        }}
        onExternalStop={abortStream}
        externalIsStreaming={isStreaming}
        externalUsage={usage}
        externalPaused={paused}
        externalQueued={queued}
        onPause={pause}
        onResume={resume}
        onLoadEarlier={loadEarlier}
        hasMore={hasMore}
        loadingEarlier={loadingEarlier}
        headerTitle="Peco"
        headerActions={headerActions}
        suggestions={[
          "介绍一下你能做什么",
          "查看我的长期记忆",
          "帮我搜索知识库",
        ]}
        welcomeMessage={
          <>
            <p className="text-lg">👋 你好！我是 Peco，你的个人 AI 助理。</p>
            <p className="text-sm mt-2">
              由 @assistant 驱动 · 我可以执行命令、管理记忆、搜索知识库。
            </p>
          </>
        }
      />

      {isWide ? (
        artifactOpen ? (
          // 焦点环与 ArtifactPanel 的 group 同口径（`ring/60` inset）：打开时焦点落在
          // 这个容器上（tabIndex=-1），没有环的话键盘用户看不出焦点去了哪。
          // `relative` 给关闭按钮当定位祖先，`flex flex-col` 让 ArtifactPanel 的
          // `flex-1` 生效（少了它高度链断，列表滚不动）；`h-full` 与父容器同高，
          // 这里是行向 flex 子项，高度由 stretch 决定，`min-h-0` 不起作用。
          <aside
            ref={artifactAsideRef}
            id="artifact-panel-aside"
            data-slot="artifact-drawer"
            tabIndex={-1}
            aria-label="产物"
            onKeyDown={onAsideKeyDown}
            className="relative flex h-full w-[480px] shrink-0 flex-col border-l rounded-l-lg shadow-lg bg-background focus:ring-2 focus:ring-ring/60 focus:ring-inset focus:outline-hidden"
          >
            <ArtifactPanel />
            <button
              type="button"
              onClick={closeArtifacts}
              className={CLOSE_BTN_CLASS}
            >
              <XIcon className="size-4" aria-hidden="true" />
              <span className="sr-only">关闭</span>
            </button>
          </aside>
        ) : null
      ) : (
        <Sheet open={artifactOpen} onOpenChange={setArtifactOpen}>
          <SheetContent
            id="artifact-panel-sheet"
            side="right"
            showCloseButton={false}
            className="w-[min(92vw,480px)] rounded-l-lg sm:max-w-none"
            aria-describedby={undefined}
            onCloseAutoFocus={(event) => {
              // 抽屉不是 Radix Trigger 打开的，Radix 手里没有 triggerRef、
              // 默认归还无处可去，关掉后焦点会掉到 body
              event.preventDefault();
              artifactTriggerRef.current?.focus();
            }}
          >
            <SheetTitle className="sr-only">产物</SheetTitle>
            <ArtifactPanel />
            <SheetClose className={CLOSE_BTN_CLASS}>
              <XIcon className="size-4" aria-hidden="true" />
              <span className="sr-only">关闭</span>
            </SheetClose>
          </SheetContent>
        </Sheet>
      )}
    </div>
  );
}
