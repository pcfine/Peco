// 团队能力配置面：头部 + tab + 内容。`/teams/:teamId` 与 Peco 翻转面共用这一份 ——
// 两处差别只在「有没有绑定空间」「能不能再装配空间」以及页面级导航。

import type { ReactNode } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
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

const TAB_ORDER = [
  "agents",
  "skills",
  "mcp",
  "knowledge",
  "spaces",
  "memory",
] as const;

type TeamTab = (typeof TAB_ORDER)[number];

const TAB_LABELS: Record<TeamTab, string> = {
  agents: "agents",
  skills: "skills",
  mcp: "MCP",
  knowledge: "知识库",
  spaces: "绑定空间",
  memory: "记忆",
};

const RESOURCE_TABS: readonly TeamTab[] = [
  "agents",
  "skills",
  "mcp",
  "knowledge",
];

/** 存档里可能是被隐藏的 tab（Peco 面没有「绑定空间」），回落到第一个。 */
function parseTab(raw: string | null, tabs: readonly TeamTab[]): TeamTab {
  return tabs.includes(raw as TeamTab) ? (raw as TeamTab) : tabs[0];
}

interface TeamConfigPanelProps {
  teamId: string;
  /** 资源 tab 的 `X-Peco-Space`：哪个已打开空间的能力根。空表示没有可用空间。 */
  wsId: string | null;
  /** 配置面藏掉「绑定空间」：Peco 不能再建空间，别暗示可以。 */
  hideSpaces?: boolean;
  hideAssemble?: boolean;
  /** 没有可用空间时资源 tab 的占位；缺省是团队页那句「先打开一个绑定空间」。 */
  noSpaceNotice?: ReactNode;
  /** `/teams/:teamId` 出错时要一条回团队列表的路；翻转面不需要。 */
  showBackToList?: boolean;
  /**
   * 标题的标签层级。`/peco` 把它当配置面挂在已有 h1 的页面里，
   * 那里必须是 h2，否则一页两个 h1。
   */
  headingLevel?: "h1" | "h2";
  /**
   * 提供时「记忆」tab 浏览这个空间的记忆；缺省走团队页的占位。
   *
   * Peco 面传 Peco 空间 —— 那里的记忆就是这一个空间的记忆，摆团队维度的占位
   * 等于让用户在两处看到同一句「尚未接入」。
   */
  memoryWsId?: string | null;
}

export function TeamConfigPanel({
  teamId,
  wsId,
  hideSpaces = false,
  hideAssemble = false,
  noSpaceNotice,
  showBackToList = false,
  headingLevel = "h1",
  memoryWsId = null,
}: TeamConfigPanelProps) {
  const navigate = useNavigate();
  const [searchParams, setSearchParams] = useSearchParams();
  const tabs = hideSpaces
    ? TAB_ORDER.filter((item) => item !== "spaces")
    : TAB_ORDER;
  const tab = parseTab(searchParams.get("tab"), tabs);
  const team = useTeam(teamId);

  const setTab = (next: TeamTab) => {
    const params = new URLSearchParams(searchParams);
    params.set("tab", next);
    setSearchParams(params, { replace: true });
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
  const Heading = headingLevel;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="shrink-0 border-b border-line px-3 py-3">
        <div className="flex h-8 items-center gap-2">
          <Heading className="truncate text-xl font-semibold text-ink">
            {detail.display_name}
          </Heading>
          <span className="shrink-0 font-mono text-2xs text-ink-faint">
            {detail.id}
          </span>
          {!hideAssemble && (
            <div className="ml-auto flex items-center gap-1">
              <Button
                size="sm"
                className="h-7"
                onClick={() =>
                  navigate(`/spaces/new?team=${encodeURIComponent(detail.id)}`)
                }
              >
                装配为空间
              </Button>
            </div>
          )}
        </div>
        <div className="mt-1 flex h-5 items-center gap-2 truncate font-mono text-mono-sm text-ink-faint">
          {detail.root}
        </div>
        {!detail.has_master && (
          <div className="mt-1 flex h-6 items-center gap-2 border-l-2 border-danger pl-2 text-xs text-danger">
            这个团队没有 `@master`，Peco 无法向它派发任务。
          </div>
        )}
      </header>

      <div
        role="tablist"
        className="flex h-9 shrink-0 items-stretch gap-1 overflow-x-auto border-b border-line"
      >
        {tabs.map((item) => (
          <button
            key={item}
            type="button"
            role="tab"
            aria-selected={tab === item}
            onClick={() => setTab(item)}
            className={cn(
              "my-1 shrink-0 rounded-[4px] px-3 text-sm",
              tab === item
                ? "bg-line text-ink"
                : "bg-transparent text-ink-muted hover:bg-line/40",
            )}
          >
            {TAB_LABELS[item]}
          </button>
        ))}
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {RESOURCE_TABS.includes(tab) &&
          (wsId ? null : (
            <div className="p-3">
              {noSpaceNotice ?? (
                <EmptyState title="这个团队还没有已打开的空间。先打开一个绑定空间，才能读写它的能力根。" />
              )}
            </div>
          ))}

        {tab === "agents" && wsId && <TeamAgentsTab wsId={wsId} />}
        {tab === "skills" && wsId && <TeamSkillsTab wsId={wsId} />}
        {tab === "mcp" && wsId && <TeamMcpTab wsId={wsId} />}
        {tab === "knowledge" && wsId && (
          <TeamKnowledgeTab wsId={wsId} detail={detail} />
        )}
        {tab === "spaces" && <TeamSpacesTab spaces={detail.spaces} />}
        {tab === "memory" &&
          (memoryWsId ? (
            <SpaceMemoryTab wsId={memoryWsId} />
          ) : (
            <TeamMemoryTab />
          ))}
      </div>
    </div>
  );
}
