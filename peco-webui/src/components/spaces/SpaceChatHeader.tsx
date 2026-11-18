// 对话列头部：状态点 + 工作名称 + 团队 + 图标组，一行 48px。
//
// 两个折叠入口贴着头的两端：「收起对话」在最左，「展开右栏」在最右。
// 两道边槽同宽，中间内容行与正文共用 CHAT_MEASURE —— 一边收放时按钮不挪位，
// 标题、消息与 composer 也继续落在同一条竖线上。名称可能很长而团队名很短，
// 所以截断落在 h1 上、团队名整体让位；图标组不随名称长短漂移。

import type { ReactNode, Ref } from "react";
import { CircleChevronLeft, Eraser } from "lucide-react";
import { Link } from "react-router-dom";
import { CHAT_MEASURE } from "@/components/chat/measure";
import { SpaceHeaderIconButton } from "@/components/spaces/SpaceHeaderIconButton";
import {
  SpaceStatusMark,
  spaceGroupOf,
} from "@/components/layout/SpaceStatusMark";
import { spaceLabel } from "@/components/layout/spaceLabel";
import type { Space } from "@/types/daemon";

interface SpaceChatHeaderProps {
  /**
   * 只要这几项：Peco 页在空间清单到手前用 Peco 身份顶上，也得能画出这一行
   * —— 唯一的 h1 与三个动作都长在这里。
   */
  space: Pick<
    Space,
    "name" | "label" | "work_dir" | "team_id" | "status" | "closed_at"
  >;
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
  /** 页面自己的动作，排在「清除对话」之前。 */
  extraActions?: ReactNode;
  /** 覆盖标题：空间名来自目录，Peco 页要的是品牌名而不是那个小写目录名。 */
  title?: string;
  /**
   * 标题的补充说明，挂到 h1 的 `title` 上：工作目录这类长字符串要能悬停看全，
   * 又不该挤进标题（不改变文案，也不改变可访问名）。
   */
  titleHint?: string;
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
  extraActions,
  title,
  titleHint,
}: SpaceChatHeaderProps) {
  return (
    <header className="flex h-11 shrink-0 items-center border-b border-line px-4">
      {/* 两道边界槽同宽（size-7 = 一枚图标按钮）：内容行因此仍以列中心为轴。 */}
      <div className="flex w-7 shrink-0 items-center">
        <SpaceHeaderIconButton
          label="收起对话"
          icon={CircleChevronLeft}
          onClick={onCollapseChat}
        />
      </div>
      <div className={`${CHAT_MEASURE} flex min-w-0 items-center gap-2`}>
        <SpaceStatusMark group={spaceGroupOf(space)} />
        <h1
          className="min-w-0 truncate text-xl font-semibold text-ink"
          title={titleHint}
        >
          {title ?? spaceLabel(space)}
        </h1>
        <Link
          to={`/teams/${encodeURIComponent(space.team_id)}`}
          title={`团队 ${space.team_id}`}
          className="tap-target shrink-0 text-caption font-medium text-ink-faint transition-colors duration-[120ms] hover:text-ink"
        >
          ·{space.team_id}
        </Link>
        <div className="ml-auto flex shrink-0 items-center gap-0.5">
          {extraActions}
          <SpaceHeaderIconButton
            label="清除对话"
            icon={Eraser}
            tone="danger"
            hint={clearDisabledReason}
            disabled={clearDisabled || clearDisabledReason !== undefined}
            onClick={onClearConversation}
          />
        </div>
      </div>
      <div className="flex w-7 shrink-0 items-center justify-end">
        {panelCollapsed && (
          <SpaceHeaderIconButton
            ref={expandPanelRef}
            label="展开右栏"
            icon={CircleChevronLeft}
            onClick={onExpandPanel}
          />
        )}
      </div>
    </header>
  );
}
