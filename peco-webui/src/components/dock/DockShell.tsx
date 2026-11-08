// Peco dock：shell ≥1184 是右列，900–1183 是底部停靠条（收起 40 / 展开 min(45vh,420)），
// 视口 <900 是覆盖式底部 Sheet。对话状态在组件内部，所以三形态共用同一个 aside ——
// 换形态只换 id 与样式，子树不重挂，在途那一轮不会随形态切换断掉。

import { useEffect, useRef } from "react";
import { ChevronDown, ChevronUp } from "lucide-react";
import { ConnectionStatus } from "@/components/common/ConnectionStatus";
import { DockArtifacts } from "@/components/dock/DockArtifacts";
import { DockChat } from "@/components/dock/DockChat";
import { useDockConversation } from "@/hooks/useDockConversation";
import { useViewportWidth } from "@/hooks/useViewportWidth";
import { useConnectionStore } from "@/stores/connectionStore";
import { useDockStore } from "@/stores/dockStore";
import { railWidth, useUiStore } from "@/stores/uiStore";
import { cn } from "@/lib/utils";

type DockForm = "column" | "bottom" | "sheet";

/** 三形态各有一个 id：底部停靠时 `#peco-dock` 只能指右列，同一串 id 不许跨形态漂移。 */
const DOCK_ID: Record<DockForm, string> = {
  column: "peco-dock",
  bottom: "peco-dock-bottom",
  sheet: "peco-dock-sheet",
};

/**
 * dock 形态只由「视口 − rail」判定：容器宽已经把 dock 自己算进去，用它判定会自指。
 * 阈值 1184 与 CSS 的 `@min-[1184px]/shell` 同值。
 */
function useDockForm(): DockForm {
  const viewport = useViewportWidth();
  const railOpen = useUiStore((state) => state.railOpen);
  if (viewport < 900) return "sheet";
  return viewport - railWidth(viewport, railOpen) >= 1184 ? "column" : "bottom";
}

/** Tab 循环只在自绘的模态层里需要；Sheet/Dialog 由 radix 管。 */
const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), textarea:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])';

export function DockShell() {
  const form = useDockForm();
  const open = useDockStore((state) => state.open);
  const setOpen = useDockStore((state) => state.setOpen);
  const tab = useDockStore((state) => state.tab);
  const setTab = useDockStore((state) => state.setTab);
  const { agentId, wsId, convId, error } = useDockConversation(open);
  const eventsState = useConnectionStore((state) => state.events);
  const chatState = useConnectionStore((state) => state.chat);
  const retryEvents = useConnectionStore((state) => state.retryEvents);
  const containerRef = useRef<HTMLElement>(null);
  const chatTabRef = useRef<HTMLButtonElement>(null);
  const collapsedButtonRef = useRef<HTMLButtonElement>(null);
  const openRef = useRef(open);

  const sheet = form === "sheet";
  // 底部停靠条的收起态是「一条 40px 的头」，不是「没有 dock」。
  const collapsedBar = form === "bottom" && !open;
  // 渲染期就写：卸载时的 cleanup 早于本轮 effect，那里要的是本次渲染的 open。
  openRef.current = open;

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

  // 覆盖式 Sheet 是自绘的模态层：焦点移进来、Tab 不跑出去、关掉还回原处。
  useEffect(() => {
    if (!sheet || !open) return;
    const node = containerRef.current;
    if (!node) return;
    const previous = document.activeElement as HTMLElement | null;
    node.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Tab") return;
      const focusable = Array.from(
        node.querySelectorAll<HTMLElement>(FOCUSABLE),
      ).filter((el) => el.offsetParent !== null);
      const first = focusable.at(0);
      const last = focusable.at(-1);
      if (!first || !last) {
        event.preventDefault();
        return;
      }
      if (
        event.shiftKey &&
        (document.activeElement === first || document.activeElement === node)
      ) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    node.addEventListener("keydown", onKeyDown);
    return () => {
      node.removeEventListener("keydown", onKeyDown);
      // 换形态时 dock 还开着，焦点仍归 dock —— 只有关掉才还回去。
      if (!openRef.current) previous?.focus?.();
    };
  }, [sheet, open]);

  // 底部条的收起/展开换的是整条头，焦点得跟着走：展开落到第一个 tab（不是 composer，
  // 那会打断用户正在输入），收起落回那条可点的头，别掉到 body 上。
  const wasCollapsed = useRef(collapsedBar);
  useEffect(() => {
    if (form === "bottom" && wasCollapsed.current !== collapsedBar) {
      if (collapsedBar) collapsedButtonRef.current?.focus();
      else chatTabRef.current?.focus();
    }
    wasCollapsed.current = collapsedBar;
  }, [form, collapsedBar]);

  if (!open && form !== "bottom") return null;

  const dockId = DOCK_ID[form];

  const header = (
    <div className="flex h-9 shrink-0 items-center gap-1 border-b border-line px-2">
      <button
        ref={chatTabRef}
        type="button"
        onClick={() => setTab("chat")}
        aria-pressed={tab === "chat"}
        className={cn(
          "h-6 rounded-[4px] px-2 text-xs",
          tab === "chat" ? "bg-line text-ink" : "text-ink-muted hover:text-ink",
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
      <span className="ml-auto flex items-center gap-2">
        <ConnectionStatus state={eventsState} label="事件" />
        <ConnectionStatus state={chatState} label="对话" />
      </span>
      {eventsState.kind === "failed" && (
        <button
          type="button"
          onClick={retryEvents}
          className="h-6 shrink-0 rounded-[4px] px-1.5 text-caption font-medium text-primary-ink hover:underline"
        >
          重试
        </button>
      )}
      {form === "bottom" && (
        <button
          type="button"
          onClick={() => setOpen(false)}
          aria-label="收起 Peco 对话"
          className="grid h-6 w-6 shrink-0 place-items-center rounded-[4px] text-ink-faint hover:bg-line hover:text-ink"
        >
          <ChevronDown aria-hidden="true" className="h-4 w-4" />
        </button>
      )}
    </div>
  );

  const content = (
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
  );

  // 收起态只有一条头：展开才挂对话，收起时不留后台树。
  const collapsedHead = (
    <button
      ref={collapsedButtonRef}
      type="button"
      onClick={() => setOpen(true)}
      aria-expanded={false}
      className="flex h-full w-full items-center gap-2 px-3 text-left hover:bg-line/40"
    >
      <span
        aria-hidden="true"
        className="size-2 shrink-0 rounded-full bg-brand"
      />
      <span className="shrink-0 text-xs text-ink">Peco 对话</span>
      <span className="ml-auto flex items-center gap-2">
        <ConnectionStatus state={eventsState} label="事件" />
        <ConnectionStatus state={chatState} label="对话" />
      </span>
      <ChevronUp
        aria-hidden="true"
        className="h-4 w-4 shrink-0 text-ink-faint"
      />
    </button>
  );

  return (
    <>
      {sheet && (
        <div
          key="backdrop"
          aria-hidden="true"
          onClick={() => setOpen(false)}
          className="fixed inset-0 z-40 bg-black/50"
        />
      )}
      <aside
        key="dock"
        id={dockId}
        ref={containerRef}
        role={sheet ? "dialog" : undefined}
        aria-modal={sheet ? true : undefined}
        aria-label={sheet ? "Peco 对话" : undefined}
        tabIndex={sheet ? -1 : undefined}
        // 宽度由所在列/行决定，改动瞬时生效：宽度是布局属性，不做动画。
        className={cn(
          "flex flex-col bg-raised",
          form === "column" &&
            "h-full w-full overflow-hidden border-l border-line-strong",
          form === "bottom" &&
            "w-full overflow-hidden border-t border-line-strong",
          collapsedBar && "h-10",
          sheet &&
            "fixed inset-x-0 bottom-0 z-50 h-[min(80vh,560px)] overflow-hidden overscroll-contain border-t border-line-strong",
        )}
        style={
          form === "bottom" && open ? { height: "min(45vh, 420px)" } : undefined
        }
      >
        {collapsedBar ? (
          collapsedHead
        ) : (
          <>
            {header}
            {content}
          </>
        )}
      </aside>
    </>
  );
}
