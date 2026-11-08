// `/teams` 团队卡片：卡片只要数字，明细在 `/teams/:teamId`。
// 计数由 `GET /api/teams` 一次带回来，不再逐团队查一遍详情。

import { Link } from "react-router-dom";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { errorText } from "@/lib/api";
import { useTeams } from "@/queries/teams";
import type { Team } from "@/types/daemon";

/** 后端未给出计数时的占位：`0` 是事实，「–」只是「还没告诉我」。 */
const MISSING = "–";

const STATS = [
  { key: "agent", label: "agents", of: (team: Team) => team.agent_count },
  { key: "skill", label: "skills", of: (team: Team) => team.skill_count },
  { key: "mcp", label: "MCP", of: (team: Team) => team.mcp_count },
  { key: "space", label: "空间", of: (team: Team) => team.space_count },
] as const;

function TeamCard({ team }: { team: Team }) {
  return (
    <li data-team-id={team.team_id} className="min-w-0">
      <Link
        to={`/teams/${encodeURIComponent(team.team_id)}`}
        className="flex h-full min-w-0 flex-col gap-1.5 rounded-[6px] bg-panel p-3 hover:bg-raised"
      >
        <span className="flex min-w-0 items-center gap-2">
          <span className="min-w-0 truncate text-sm font-semibold text-ink">
            {team.display_name}
          </span>
          <span className="ml-auto shrink-0 tabular-nums text-2xs text-ink-faint">
            v{team.template_version}
          </span>
        </span>
        <span className="truncate font-mono text-2xs text-ink-faint">
          {team.team_id}
        </span>
        <span className="mt-auto flex flex-wrap items-baseline gap-x-3 gap-y-1 pt-1 text-caption font-medium">
          {STATS.map((stat) => (
            <span key={stat.key} className="flex items-baseline gap-1">
              <span className="text-ink-faint">{stat.label}</span>
              <span data-stat={stat.key} className="tabular-nums text-ink">
                {stat.of(team) ?? MISSING}
              </span>
            </span>
          ))}
        </span>
      </Link>
    </li>
  );
}

export function TeamListPage() {
  const { data: teams, error, isPending, refetch } = useTeams();

  return (
    <div className="space-y-4">
      {error && (
        <ErrorBanner
          message={errorText(error)}
          onRetry={() => void refetch()}
        />
      )}

      {isPending && (
        <div className="grid grid-cols-1 gap-3 @min-[640px]/work:grid-cols-2 @min-[1024px]/work:grid-cols-3">
          {[0, 1, 2].map((card) => (
            <Skeleton key={card} className="h-24 w-full" />
          ))}
        </div>
      )}

      {teams && teams.length === 0 && (
        <EmptyState title="只有一个 Peco 内置团队。" />
      )}

      <ul className="board-grid grid grid-cols-1 @min-[640px]/work:grid-cols-2 @min-[1024px]/work:grid-cols-3">
        {(teams ?? []).map((team) => (
          <TeamCard key={team.team_id} team={team} />
        ))}
      </ul>
    </div>
  );
}
