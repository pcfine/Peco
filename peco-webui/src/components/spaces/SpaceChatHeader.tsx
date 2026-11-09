// 对话列头部：状态点 + 工作名称 + 团队 + 图标组，一行 48px。
//
// 名称可能很长而团队名很短，所以截断落在 h1 上、团队名整体让位；
// 图标组永远靠右，不随名称长短漂移。整行与聊天正文同宽同轴：标题、消息与
// composer 落在同一条竖线上，两侧留白随列宽收放。

import type { Ref } from "react";
import { Eraser, PanelLeftClose, PanelRightOpen } from "lucide-react";
import { Link } from "react-router-dom";
import { SpaceHeaderIconButton } from "@/components/spaces/SpaceHeaderIconButton";
import {
  SpaceStatusMark,
  spaceGroupOf,
} from "@/components/layout/SpaceStatusMark";
import { spaceLabel } from "@/components/layout/spaceLabel";
import type { Space } from "@/types/daemon";

interface SpaceChatHeaderProps {
  space: Space;
  /** 右栏收起时这里接一个「展开右栏」：两块互相是对方的回来路径。 */
  panelCollapsed: boolean;
  onExpandPanel: () => void;
  onCollapseChat: () => void;
  /** 折叠后焦点要落到这枚按钮上，ref 由页面持有。 */
  expandPanelRef?: Ref<HTMLButtonElement>;
  onClearConversation: () => void;
  /** 会话 id 未就绪时按钮按不动，但仍留着位置与说明。 */
  clearDisabled: boolean;
  /**
   * 现在不能清的其它原因（有一轮在跑）。给值就按不动并把原因挂到 Tooltip 上 ——
   * 用户不必点一次、吃一个 409 才知道。
   */
  clearDisabledReason?: string;
}

export function SpaceChatHeader({
  space,
  panelCollapsed,
  onExpandPanel,
  onCollapseChat,
  expandPanelRef,
  onClearConversation,
  clearDisabled,
  clearDisabledReason,
}: SpaceChatHeaderProps) {
  return (
    <header className="flex h-12 shrink-0 items-center border-b border-line px-3">
      <div className="mx-auto flex w-full max-w-chat min-w-0 items-center gap-2">
        <SpaceStatusMark group={spaceGroupOf(space)} />
        <h1 className="min-w-0 truncate text-xl font-semibold text-ink">
          {spaceLabel(space)}
        </h1>
        <Link
          to={`/teams/${encodeURIComponent(space.team_id)}`}
          title={`团队 ${space.team_id}`}
          className="tap-target shrink-0 text-caption font-medium text-ink-faint transition-colors duration-[120ms] hover:text-ink"
        >
          ·{space.team_id}
        </Link>
        <div className="ml-auto flex shrink-0 items-center gap-0.5">
          <SpaceHeaderIconButton
            label="清除对话"
            icon={Eraser}
            tone="danger"
            hint={clearDisabledReason}
            disabled={clearDisabled || clearDisabledReason !== undefined}
            onClick={onClearConversation}
          />
          {panelCollapsed && (
            <SpaceHeaderIconButton
              ref={expandPanelRef}
              label="展开右栏"
              icon={PanelRightOpen}
              onClick={onExpandPanel}
            />
          )}
          <SpaceHeaderIconButton
            label="收起对话"
            icon={PanelLeftClose}
            onClick={onCollapseChat}
          />
        </div>
      </div>
    </header>
  );
}
