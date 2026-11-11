// Peco 对话抽屉：贴在左栏右缘，覆盖在内容之上 —— 不占列、不占行、不加遮罩，
// 页面开着抽屉仍然可读。开关是左栏那枚品牌按钮（<900 时是顶栏的图标）。
//
// 内容只在首次打开后挂载，之后不再卸载：开合频繁，卸载会丢滚动位置与在途那一轮。

import { useEffect, useRef, useState } from "react";
import { X } from "lucide-react";
import { ConnectionStatus } from "@/components/common/ConnectionStatus";
import { DockArtifacts } from "@/components/dock/DockArtifacts";
import { DockChat } from "@/components/dock/DockChat";
import { useDockConversation } from "@/hooks/useDockConversation";
import { useViewportWidth } from "@/hooks/useViewportWidth";
import { useConnectionStore } from "@/stores/connectionStore";
import { useDockStore } from "@/stores/dockStore";
import { railWidth, useUiStore } from "@/stores/uiStore";
import { DOCK_DRAWER_WIDTH } from "@/components/layout/shellMetrics";
import { cn } from "@/lib/utils";

export function DockShell() {
  const viewport = useViewportWidth();
  const railOpen = useUiStore((state) => state.railOpen);
  const open = useDockStore((state) => state.open);
  const setOpen = useDockStore((state) => state.setOpen);
  const tab = useDockStore((state) => state.tab);
  const setTab = useDockStore((state) => state.setTab);
  // 从没打开过就不该白建一条对话与 SSE。
  const [everOpened, setEverOpened] = useState(open);
  const live = open || everOpened;
  const { agentId, wsId, convId, error } = useDockConversation(live);
  const eventsState = useConnectionStore((state) => state.events);
  const chatState = useConnectionStore((state) => state.chat);
  const retryEvents = useConnectionStore((state) => state.retryEvents);
  const panelRef = useRef<HTMLElement>(null);
  const returnFocusTo = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (open) setEverOpened(true);
  }, [open]);

  // 打开时焦点进面板；关掉时还回触发它的按钮。`inert` 会把焦点挤到 body，
  // 所以 body 也算「焦点还在抽屉里」；用户自己挪到别处的不抢。
  // 首屏就是打开态（存档）不动焦点：那是「加载」，不是「打开」。
  const wasOpen = useRef(open);
  useEffect(() => {
    const opened = open && !wasOpen.current;
    wasOpen.current = open;
    if (!opened) return;
    const node = panelRef.current;
    if (!node) return;
    returnFocusTo.current = document.activeElement as HTMLElement | null;
    node.focus();
    return () => {
      const focused = document.activeElement;
      const stillHere =
        focused === node || focused === document.body || node.contains(focused);
      if (stillHere) returnFocusTo.current?.focus?.();
    };
  }, [open]);

  // Esc 只在焦点落进抽屉时关它，否则会把页面上的 Esc（Dialog 等）抢走。
  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (!panelRef.current?.contains(document.activeElement)) return;
      setOpen(false);
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [open, setOpen]);

  const content = (
    <div className="min-h-0 flex-1 overflow-hidden">
      {error ? (
        <p className="max-w-narrow p-3 text-sm text-ink-muted">
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
  );

  return (
    <aside
      id="peco-dock"
      ref={panelRef}
      tabIndex={-1}
      aria-label="Peco 对话"
      aria-hidden={open ? undefined : true}
      // 关着的抽屉在视野外，但键盘与点击都不该能进去。
      inert={!open}
      className={cn(
        "fixed inset-y-0 z-40 flex flex-col overscroll-contain border-r border-line-strong bg-raised",
        open ? "translate-x-0" : "-translate-x-full pointer-events-none",
      )}
      // 抽屉起点跟着左栏宽度走，且必须与左栏同步做 300ms 动画：左栏是 flex 布局里
      // 连续变宽的，left 若瞬间跳到终点，收起态停在视野外的抽屉右缘就会从正在变宽
      // 的左栏下方露出一条（头部右侧的连接状态）。
      // 曲线必须写字面量：Tailwind 的 ease-out 是 cubic-bezier(0,0,0.2,1)，CSS 关键字
      // ease-out 是 cubic-bezier(0,0,0.58,1)，混用会让两条动画中途错开近 30px。
      // 开合走 translate（Tailwind v4 的 translate-x-* 不再写进 transform）。
      style={{
        left: railWidth(viewport, railOpen),
        width: `min(92vw, ${DOCK_DRAWER_WIDTH}px)`,
        transition:
          "left 300ms cubic-bezier(0, 0, 0.2, 1), translate 120ms cubic-bezier(0, 0, 0.2, 1)",
      }}
    >
      <div className="flex h-11 shrink-0 items-center gap-1 border-b border-line px-2">
        <button
          type="button"
          onClick={() => setTab("chat")}
          aria-pressed={tab === "chat"}
          className={cn(
            "h-7 rounded-full border px-2.5 text-xs",
            tab === "chat"
              ? "border-brand/40 bg-brand/10 text-ink"
              : "border-transparent text-ink-muted hover:bg-line/50 hover:text-ink",
          )}
        >
          对话
        </button>
        <button
          type="button"
          onClick={() => setTab("artifacts")}
          aria-pressed={tab === "artifacts"}
          className={cn(
            "h-7 rounded-full border px-2.5 text-xs",
            tab === "artifacts"
              ? "border-brand/40 bg-brand/10 text-ink"
              : "border-transparent text-ink-muted hover:bg-line/50 hover:text-ink",
          )}
        >
          产物
        </button>
        {/* 两条流分开标：事件断了不代表正在生成的这一轮断了。 */}
        <span className="ml-auto flex items-center gap-2">
          <ConnectionStatus state={eventsState} label="事件" />
          <ConnectionStatus state={chatState} label="对话" />
        </span>
        {eventsState.kind === "failed" && (
          <button
            type="button"
            onClick={retryEvents}
            className="h-6 shrink-0 rounded-sm px-1.5 text-caption font-medium text-primary-ink hover:underline"
          >
            重试
          </button>
        )}
        <button
          type="button"
          onClick={() => setOpen(false)}
          aria-label="关闭 Peco 对话"
          className="grid h-6 w-6 shrink-0 place-items-center rounded-sm text-ink-faint hover:bg-line hover:text-ink"
        >
          <X aria-hidden="true" className="h-4 w-4" />
        </button>
      </div>
      {live && content}
    </aside>
  );
}
