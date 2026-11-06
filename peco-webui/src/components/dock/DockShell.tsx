// Peco dock：宽屏是右列（420 / 360），窄屏是底部 Sheet。
// 从宽到窄只换形态不关面板 —— 对话状态在组件内部，所以形态切换必须保住同一个子树。

import { useEffect, useRef } from "react";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { ConnectionStatus } from "@/components/common/ConnectionStatus";
import { DockArtifacts } from "@/components/dock/DockArtifacts";
import { DockChat } from "@/components/dock/DockChat";
import { useDockConversation } from "@/hooks/useDockConversation";
import { useViewportWidth } from "@/hooks/useViewportWidth";
import { useConnectionStore } from "@/stores/connectionStore";
import { useDockStore } from "@/stores/dockStore";
import { useUiStore } from "@/stores/uiStore";
import { cn } from "@/lib/utils";

/** 右列宽的档位：420 / 360 / 0（底部 Sheet）。 */
function dockWidth(rightColumn: number): 420 | 360 | 0 {
  if (rightColumn >= 1040) return 420;
  if (rightColumn >= 720) return 360;
  return 0;
}

export function DockShell() {
  const viewport = useViewportWidth();
  const railOpen = useUiStore((state) => state.railOpen);
  const wide = viewport >= 900;
  const rightColumn = viewport - (wide ? (railOpen ? 256 : 64) : 0);
  const width = dockWidth(rightColumn);

  const open = useDockStore((state) => state.open);
  const setOpen = useDockStore((state) => state.setOpen);
  const tab = useDockStore((state) => state.tab);
  const setTab = useDockStore((state) => state.setTab);
  const { agentId, wsId, convId, error } = useDockConversation(open);
  const eventsState = useConnectionStore((state) => state.events);
  const chatState = useConnectionStore((state) => state.chat);
  const retryEvents = useConnectionStore((state) => state.retryEvents);
  const containerRef = useRef<HTMLDivElement>(null);

  // Esc 只在焦点落进 dock 时关它，否则会把页面上的 Esc（Dialog 等）抢走。
  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (!containerRef.current?.contains(document.activeElement)) return;
      setOpen(false);
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [open, setOpen]);

  if (!open) return null;

  const body = (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex h-9 shrink-0 items-center gap-1 border-b border-line px-2">
        <button
          type="button"
          onClick={() => setTab("chat")}
          aria-pressed={tab === "chat"}
          className={cn(
            "h-6 rounded-[4px] px-2 text-xs",
            tab === "chat"
              ? "bg-line text-ink"
              : "text-ink-muted hover:text-ink",
          )}
        >
          对话
        </button>
        <button
          type="button"
          onClick={() => setTab("artifacts")}
          aria-pressed={tab === "artifacts"}
          className={cn(
            "h-6 rounded-[4px] px-2 text-xs",
            tab === "artifacts"
              ? "bg-line text-ink"
              : "text-ink-muted hover:text-ink",
          )}
        >
          产物
        </button>
        {/* 两条流分开标：事件断了不代表正在生成的这一轮断了。 */}
        <ConnectionStatus
          state={eventsState}
          label="事件"
          className="ml-auto"
        />
        <ConnectionStatus state={chatState} label="对话" />
        {eventsState.kind === "failed" && (
          <button
            type="button"
            onClick={retryEvents}
            className="h-6 shrink-0 rounded-[4px] px-1.5 text-2xs text-primary-ink hover:underline"
          >
            重试
          </button>
        )}
      </div>
      <div className="min-h-0 flex-1 overflow-hidden">
        {error ? (
          <p className="max-w-[480px] p-3 text-sm text-ink-muted">
            Peco 空间还没打开，无法开始对话。
          </p>
        ) : !convId || !wsId ? (
          <p className="p-3 text-sm text-ink-muted">正在准备对话…</p>
        ) : tab === "chat" ? (
          <DockChat agentId={agentId} convId={convId} wsId={wsId} />
        ) : (
          <div className="h-full overflow-y-auto">
            <DockArtifacts agentId={agentId} convId={convId} wsId={wsId} />
          </div>
        )}
      </div>
    </div>
  );

  if (width === 0) {
    return (
      <Sheet open={open} onOpenChange={setOpen}>
        <SheetContent
          side="bottom"
          className="h-[min(80vh,560px)] gap-0 border-t border-line bg-raised p-0"
        >
          <SheetTitle className="sr-only">Peco 对话</SheetTitle>
          <div ref={containerRef} className="flex h-full min-h-0 flex-col">
            {body}
          </div>
        </SheetContent>
      </Sheet>
    );
  }

  return (
    <aside
      id="peco-dock"
      ref={containerRef}
      style={{ width }}
      className="flex h-full shrink-0 flex-col overflow-hidden border-l border-line bg-raised transition-[width] duration-[120ms]"
    >
      {body}
    </aside>
  );
}
