// 团队能力配置面：头部 + 左侧分类栏 + 内容区。`/teams/:teamId` 与 Peco 翻转面共用这一份 ——
// 两处差别只在「有没有绑定空间」「能不能再装配空间」以及页面级导航。

import { Fragment, type ComponentType, type ReactNode } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import {
  BookOpen,
  Bot,
  BrainCircuit,
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

/** 分类栏的两个分组；组内顺序就是导航顺序，`TAB_ORDER` 也从这里展开。 */
const TAB_GROUPS: readonly { title: string; items: readonly TeamTab[] }[] = [
  { title: "能力", items: RESOURCE_TABS },
  { title: "其他", items: ["spaces", "memory"] },
];

const TAB_ORDER: readonly TeamTab[] = TAB_GROUPS.flatMap(
  (group) => group.items,
);

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
    <div className="@container/team flex h-full min-h-0 flex-col">
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

      <div className="flex min-h-0 flex-1">
        <nav
          role="tablist"
          aria-orientation="vertical"
          className="flex w-44 shrink-0 flex-col gap-0.5 overflow-y-auto border-r border-line bg-rail/50 px-2 py-2 @max-[560px]/team:w-12 @max-[560px]/team:px-1"
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
                      "flex h-8 shrink-0 items-center gap-2 rounded-[6px] px-2 text-[13px] @max-[560px]/team:justify-center @max-[560px]/team:px-0",
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

        <div
          role="tabpanel"
          id={`team-panel-${tab}`}
          aria-labelledby={`team-tab-${tab}`}
          className="flex min-h-0 min-w-0 flex-1 flex-col"
        >
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
      </div>
    </div>
  );
}
