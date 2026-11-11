// 团队配置页：左侧分类栏（顶部是团队身份）+ 内容区，整页就是这两栏。
// 左侧只放导航；「新建空间」是绑定空间内容区的头部动作。
// `/teams/peco` 也走这一份 —— Peco 是保留团队，差别只在「没有绑定空间可看」，
// 由调用方用 hideSpaces 表达。

import { Fragment, type ComponentType, type ReactNode } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import {
  BookOpen,
  Bot,
  BrainCircuit,
  ChevronLeft,
  FolderOpen,
  Plug,
  Puzzle,
} from "lucide-react";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { SpaceMemoryTab } from "@/components/spaces/SpaceMemoryTab";
import { Skeleton } from "@/components/ui/skeleton";
import { Button } from "@/components/ui/button";
import { TeamAgentsTab } from "@/components/teams/TeamAgentsTab";
import { TeamKnowledgeTab } from "@/components/teams/TeamKnowledgeTab";
import { TeamMcpTab } from "@/components/teams/TeamMcpTab";
import { TeamMemoryTab } from "@/components/teams/TeamMemoryTab";
import { TeamSkillsTab } from "@/components/teams/TeamSkillsTab";
import { TeamSpacesTab } from "@/components/teams/TeamSpacesTab";
import { useTeam } from "@/queries/teamDetail";
import { errorText } from "@/lib/api";
import { cn } from "@/lib/utils";

type TeamTab = "agents" | "skills" | "mcp" | "knowledge" | "spaces" | "memory";

const TAB_LABELS: Record<TeamTab, string> = {
  agents: "agents",
  skills: "skills",
  mcp: "MCP",
  knowledge: "知识库",
  spaces: "绑定空间",
  memory: "记忆",
};

const TAB_ICONS: Record<TeamTab, ComponentType<{ className?: string }>> = {
  agents: Bot,
  skills: Puzzle,
  mcp: Plug,
  knowledge: BookOpen,
  spaces: FolderOpen,
  memory: BrainCircuit,
};

/** 需要「该团队已打开的空间」作请求头的那几个 tab：能力根都在空间里。 */
const RESOURCE_TABS: readonly TeamTab[] = [
  "agents",
  "skills",
  "mcp",
  "knowledge",
];

/**
 * 分类栏的两个分组；组内顺序就是导航顺序，`TAB_ORDER` 也从这里展开。
 * 「能力」把记忆收在知识库下方：两者都是看这个团队/空间沉淀了什么；
 * 「工作」下是绑定空间。
 */
const TAB_GROUPS: readonly { title: string; items: readonly TeamTab[] }[] = [
  { title: "能力", items: [...RESOURCE_TABS, "memory"] },
  { title: "工作", items: ["spaces"] },
];

const TAB_ORDER: readonly TeamTab[] = TAB_GROUPS.flatMap(
  (group) => group.items,
);

/** 存档里可能是被隐藏的 tab（Peco 团队没有「绑定空间」），回落到第一个。 */
function parseTab(raw: string | null, tabs: readonly TeamTab[]): TeamTab {
  return tabs.includes(raw as TeamTab) ? (raw as TeamTab) : tabs[0];
}

interface TeamConfigPanelProps {
  teamId: string;
  /** 资源 tab 的 `X-Peco-Space`：哪个已打开空间的能力根。空表示没有可用空间。 */
  wsId: string | null;
  /** 藏掉「绑定空间」与整个「工作」分组：Peco 是保留团队，不能再建/绑空间。 */
  hideSpaces?: boolean;
  /** 没有可用空间时资源 tab 的占位；缺省是那句「先打开一个绑定空间」。 */
  noSpaceNotice?: ReactNode;
  /** `/teams/:teamId` 出错时要一条回团队列表的路。 */
  showBackToList?: boolean;
  /**
   * 提供时「记忆」tab 浏览这个空间的记忆；缺省走团队维度的占位。
   *
   * Peco 团队传 Peco 空间 —— 那里的记忆就是这一个空间的记忆，摆团队维度的占位
   * 等于让用户在两处看到同一句「尚未接入」。
   */
  memoryWsId?: string | null;
}

export function TeamConfigPanel({
  teamId,
  wsId,
  hideSpaces = false,
  noSpaceNotice,
  showBackToList = false,
  memoryWsId = null,
}: TeamConfigPanelProps) {
  const navigate = useNavigate();
  const [searchParams, setSearchParams] = useSearchParams();
  const tabs = hideSpaces
    ? TAB_ORDER.filter((item) => item !== "spaces")
    : TAB_ORDER;
  const groups = TAB_GROUPS.map((group) => ({
    title: group.title,
    items: group.items.filter((item) => tabs.includes(item)),
  })).filter((group) => group.items.length > 0);
  const tab = parseTab(searchParams.get("tab"), tabs);
  const team = useTeam(teamId);

  const setTab = (next: TeamTab) => {
    const params = new URLSearchParams(searchParams);
    params.set("tab", next);
    setSearchParams(params, { replace: true });
  };

  // 返回上一页；直接贴 URL 打开时没有上一页，退到团队列表。
  const goBack = () => {
    const state = window.history.state as { idx?: number } | null;
    if ((state?.idx ?? 0) > 0) navigate(-1);
    else navigate("/teams");
  };

  if (team.isPending) {
    return (
      <div className="space-y-px">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-10 w-full" />
        ))}
      </div>
    );
  }

  if (team.error || !team.data) {
    return (
      <div className="space-y-3">
        <ErrorBanner
          message={errorText(team.error)}
          onRetry={() => void team.refetch()}
        />
        {showBackToList && (
          <Link
            to="/teams"
            className="tap-target -mx-2 self-start px-2 text-xs text-primary-ink hover:underline"
          >
            返回团队列表
          </Link>
        )}
      </div>
    );
  }

  const detail = team.data;

  return (
    <div className="@container/team flex h-full min-h-0">
      <div className="flex w-44 shrink-0 flex-col border-r border-line bg-rail/50 @max-[560px]/team:w-12">
        <div className="mb-1 flex h-11 items-center border-b border-line px-2 @max-[560px]/team:px-1">
          {/* 三栏等宽栅格：左右各留 28px，名称在中间那栏居中，返回键随
              `items-center` 垂直居中。 */}
          <div className="grid w-full grid-cols-[28px_1fr_28px] items-center @max-[560px]/team:flex @max-[560px]/team:justify-center">
            {/* 颜色与下方导航一致（ink-muted）。 */}
            <Button
              variant="ghost"
              size="icon-sm"
              className="text-ink-muted"
              aria-label="返回上一页"
              title="返回上一页"
              onClick={goBack}
            >
              <ChevronLeft className="size-5" />
            </Button>
            {/* 窄档只剩图标栏：标题视觉隐藏但留在无障碍树里，一页仍有一个 h1。
                团队 id 不上屏 —— 路由地址里已经带着它。 */}
            <h1
              className="min-w-0 truncate text-center text-sm font-semibold text-ink @max-[560px]/team:sr-only"
              title={detail.root}
            >
              {detail.display_name}
            </h1>
            <span aria-hidden />
          </div>
        </div>

        <nav
          role="tablist"
          aria-orientation="vertical"
          className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto px-2 pb-2 @max-[560px]/team:px-1"
        >
          {groups.map((group, index) => (
            <Fragment key={group.title}>
              {index > 0 && (
                <div className="my-2 border-t border-line" role="separator" />
              )}
              <div className="px-2 py-1 text-caption font-medium text-ink-faint @max-[560px]/team:hidden">
                {group.title}
              </div>
              {group.items.map((item) => {
                const active = tab === item;
                const Icon = TAB_ICONS[item];
                return (
                  <button
                    key={item}
                    type="button"
                    role="tab"
                    id={`team-tab-${item}`}
                    aria-controls={`team-panel-${item}`}
                    aria-selected={active}
                    title={TAB_LABELS[item]}
                    aria-label={TAB_LABELS[item]}
                    onClick={() => setTab(item)}
                    className={cn(
                      "flex h-8 shrink-0 items-center gap-2 rounded-md px-2 text-[13px] @max-[560px]/team:justify-center @max-[560px]/team:px-0",
                      active
                        ? "bg-line/50 font-medium text-ink"
                        : "text-ink-muted hover:bg-line/25",
                    )}
                  >
                    <Icon
                      className={cn(
                        "size-4 shrink-0",
                        active ? "text-brand" : "text-ink-faint",
                      )}
                    />
                    <span className="truncate @max-[560px]/team:hidden">
                      {TAB_LABELS[item]}
                    </span>
                  </button>
                );
              })}
            </Fragment>
          ))}
        </nav>
      </div>

      <div
        role="tabpanel"
        id={`team-panel-${tab}`}
        aria-labelledby={`team-tab-${tab}`}
        className="flex min-h-0 min-w-0 flex-1 flex-col"
      >
        {/* skills 自己撑满高度并内部滚动（编辑器需要确定高度的双栏）；
            其余 tab 沿用整块滚动。 */}
        <div
          className={cn(
            "min-h-0 flex-1",
            tab === "skills" ? "overflow-hidden" : "overflow-y-auto",
          )}
        >
          {RESOURCE_TABS.includes(tab) &&
            (wsId ? null : (
              <div className="p-3">
                {noSpaceNotice ?? (
                  <EmptyState title="这个团队还没有已打开的空间。先打开一个绑定空间，才能读写它的能力根。" />
                )}
              </div>
            ))}

          {tab === "agents" && wsId && (
            <TeamAgentsTab
              wsId={wsId}
              teamId={detail.id}
              noMaster={!detail.has_master}
            />
          )}
          {tab === "skills" && wsId && <TeamSkillsTab wsId={wsId} />}
          {tab === "mcp" && wsId && <TeamMcpTab wsId={wsId} />}
          {tab === "knowledge" && wsId && (
            <TeamKnowledgeTab wsId={wsId} detail={detail} />
          )}
          {tab === "spaces" && (
            <TeamSpacesTab teamId={detail.id} spaces={detail.spaces} />
          )}
          {tab === "memory" &&
            (memoryWsId ? (
              <SpaceMemoryTab wsId={memoryWsId} />
            ) : (
              <TeamMemoryTab />
            ))}
        </div>
      </div>
    </div>
  );
}
