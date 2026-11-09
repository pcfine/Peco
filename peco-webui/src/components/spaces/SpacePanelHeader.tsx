// 右栏头部：三个模式 tab + 折叠开关，与对话列头部同高、border-b 连成一条线。
//
// 对话列收起后左头随它一起消失，「展开对话」只能落在这里 —— 两块各自是对方的回来路径。
// 右栏独占整行时，头部与内容按同一条 880 轴居中，否则 tab 与内容各靠一边。

import type { Ref } from "react";
import { PanelLeftOpen, PanelRightClose } from "lucide-react";
import { SpaceHeaderIconButton } from "@/components/spaces/SpaceHeaderIconButton";
import {
  SPACE_PANEL_LABELS,
  type SpacePanelMode,
} from "@/components/spaces/useSpacePanelState";
import { cn } from "@/lib/utils";

interface SpacePanelHeaderProps {
  mode: SpacePanelMode;
  /** 可选的模式集合：顺序即 tab 顺序，由变体决定，这里不猜。 */
  modes: readonly SpacePanelMode[];
  /** 在飞任务数；0 时不出角标 —— 「任务 0」比不写更吵。 */
  inflight: number;
  chatCollapsed: boolean;
  standalone: boolean;
  onSelect: (mode: SpacePanelMode) => void;
  onExpandChat: () => void;
  onCollapsePanel: () => void;
  /** 折叠后焦点要落到这枚按钮上，ref 由页面持有。 */
  expandChatRef?: Ref<HTMLButtonElement>;
}

export function SpacePanelHeader({
  mode,
  modes,
  inflight,
  chatCollapsed,
  standalone,
  onSelect,
  onExpandChat,
  onCollapsePanel,
  expandChatRef,
}: SpacePanelHeaderProps) {
  return (
    <header className="flex h-12 shrink-0 items-center border-b border-line px-3">
      <div
        className={cn(
          "flex w-full min-w-0 items-center gap-0.5",
          standalone && "mx-auto max-w-mid",
        )}
      >
        {chatCollapsed && (
          <SpaceHeaderIconButton
            ref={expandChatRef}
            label="展开对话"
            icon={PanelLeftOpen}
            onClick={onExpandChat}
          />
        )}
        <div role="tablist" aria-label="右栏" className="flex min-w-0 gap-0.5">
          {modes.map((item) => {
            const selected = mode === item;
            return (
              <button
                key={item}
                type="button"
                role="tab"
                aria-selected={selected}
                onClick={() => onSelect(item)}
                className={cn(
                  "h-7 shrink-0 rounded-[4px] px-2.5 text-body transition-colors duration-[120ms]",
                  selected
                    ? "bg-line text-ink"
                    : "text-ink-muted hover:bg-line/50",
                )}
              >
                {SPACE_PANEL_LABELS[item]}
                {item === "tasks" && inflight > 0 && (
                  <span className="ml-1 tabular-nums">{inflight}</span>
                )}
              </button>
            );
          })}
        </div>
        <div className="ml-auto flex shrink-0 items-center">
          <SpaceHeaderIconButton
            label="收起右栏"
            icon={PanelRightClose}
            onClick={onCollapsePanel}
          />
        </div>
      </div>
    </header>
  );
}
