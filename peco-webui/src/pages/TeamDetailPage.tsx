// `/teams/:teamId` —— 头部 + 6 个 tab。四个资源 tab（agents/skills/MCP/知识库）
// 都要一个「该团队已打开的空间」作 `X-Peco-Space`：团队没有已打开空间时不是
// 静默失败，而是把原因摆在 tab 里。

import {
  Link,
  useNavigate,
  useParams,
  useSearchParams,
} from "react-router-dom";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { Button } from "@/components/ui/button";
import { TeamAgentsTab } from "@/components/teams/TeamAgentsTab";
import { TeamKnowledgeTab } from "@/components/teams/TeamKnowledgeTab";
import { TeamMcpTab } from "@/components/teams/TeamMcpTab";
import { TeamMemoryTab } from "@/components/teams/TeamMemoryTab";
import { TeamSkillsTab } from "@/components/teams/TeamSkillsTab";
import { TeamSpacesTab } from "@/components/teams/TeamSpacesTab";
import { useTeam } from "@/queries/teamDetail";
import { useSpaces } from "@/queries/spaces";
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

function parseTab(raw: string | null): TeamTab {
  return TAB_ORDER.includes(raw as TeamTab) ? (raw as TeamTab) : "agents";
}

export function TeamDetailPage() {
  const { teamId = "" } = useParams();
  const navigate = useNavigate();
  const [searchParams, setSearchParams] = useSearchParams();
  const tab = parseTab(searchParams.get("tab"));
  const team = useTeam(teamId);
  const spaces = useSpaces();

  // 团队是能力边界：哪个已打开空间作请求头不影响结果，取第一个。
  const wsId =
    (spaces.data ?? []).find(
      (space) => space.team_id === teamId && space.status === "open",
    )?.ws_id ?? null;

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
        <Link to="/teams" className="text-xs text-primary-ink hover:underline">
          返回团队列表
        </Link>
      </div>
    );
  }

  const detail = team.data;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="shrink-0 border-b border-line px-3 py-3">
        <div className="flex h-8 items-center gap-2">
          <h1 className="truncate text-xl font-semibold text-ink">
            {detail.display_name}
          </h1>
          <span className="shrink-0 font-mono text-2xs text-ink-faint">
            {detail.id}
          </span>
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
        {TAB_ORDER.map((item) => (
          <button
            key={item}
            type="button"
            role="tab"
            aria-selected={tab === item}
            onClick={() => setTab(item)}
            className={cn(
              "shrink-0 border-b-2 px-3 text-sm",
              tab === item
                ? "border-brand text-ink"
                : "border-transparent text-ink-muted hover:text-ink",
            )}
          >
            {TAB_LABELS[item]}
          </button>
        ))}
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {(tab === "agents" ||
          tab === "skills" ||
          tab === "mcp" ||
          tab === "knowledge") &&
          (wsId ? null : (
            <div className="p-3">
              <EmptyState title="这个团队还没有已打开的空间。先打开一个绑定空间，才能读写它的能力根。" />
            </div>
          ))}

        {tab === "agents" && wsId && <TeamAgentsTab wsId={wsId} />}
        {tab === "skills" && wsId && <TeamSkillsTab wsId={wsId} />}
        {tab === "mcp" && wsId && <TeamMcpTab wsId={wsId} />}
        {tab === "knowledge" && wsId && (
          <TeamKnowledgeTab wsId={wsId} detail={detail} />
        )}
        {tab === "spaces" && <TeamSpacesTab spaces={detail.spaces} />}
        {tab === "memory" && <TeamMemoryTab />}
      </div>
    </div>
  );
}
