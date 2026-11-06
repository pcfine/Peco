// `/teams` 团队列表：行 h-40。skills 数只有详情接口给，逐团队并行取一次 ——
// 团队数是十几个量级，N 次请求换掉「skills 一格永远是 —」是值的。

import { Link } from "react-router-dom";
import { useQueries } from "@tanstack/react-query";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { getTeam } from "@/api/teams";
import { errorText } from "@/lib/api";
import { useTeams } from "@/queries/teams";
import type { Team } from "@/types/daemon";

function TeamRow({
  team,
  skillCount,
}: {
  team: Team;
  skillCount: number | null;
}) {
  return (
    <li className="flex h-16 flex-col justify-center gap-0.5 border-b border-line px-3 py-1 @min-[900px]/main:h-10 @min-[900px]/main:flex-row @min-[900px]/main:items-center @min-[900px]/main:gap-2 @min-[900px]/main:py-0">
      <Link
        to={`/teams/${encodeURIComponent(team.team_id)}`}
        className="min-w-0 flex-1 truncate text-sm text-ink hover:underline"
      >
        {team.display_name}
      </Link>
      <span className="truncate font-mono text-mono-sm text-ink-faint @min-[900px]/main:w-40 @min-[900px]/main:shrink-0">
        {team.team_id}
      </span>
      <span className="tabular-nums text-2xs text-ink-faint @min-[900px]/main:w-16 @min-[900px]/main:shrink-0 @min-[900px]/main:text-right">
        agents {team.agents.length}
      </span>
      <span className="tabular-nums text-2xs text-ink-faint @min-[900px]/main:w-20 @min-[900px]/main:shrink-0 @min-[900px]/main:text-right">
        skills {skillCount ?? "—"}
      </span>
      <span className="tabular-nums text-2xs text-ink-faint @min-[900px]/main:w-24 @min-[900px]/main:shrink-0 @min-[900px]/main:text-right">
        空间 {team.bound_spaces.length}
      </span>
      <span className="tabular-nums text-2xs text-ink-faint @min-[900px]/main:w-16 @min-[900px]/main:shrink-0 @min-[900px]/main:text-right">
        v{team.template_version}
      </span>
      <Link
        to={`/teams/${encodeURIComponent(team.team_id)}?tab=spaces`}
        className="shrink-0 text-2xs text-ink-muted hover:text-ink"
      >
        装配
      </Link>
    </li>
  );
}

export function TeamListPage() {
  const { data: teams, error, isPending, refetch } = useTeams();
  const details = useQueries({
    queries: (teams ?? []).map((team) => ({
      queryKey: ["team", team.team_id],
      queryFn: () => getTeam(team.team_id),
      staleTime: 60_000,
    })),
  });

  return (
    <div className="space-y-4">
      {error && (
        <ErrorBanner
          message={errorText(error)}
          onRetry={() => void refetch()}
        />
      )}

      {isPending && (
        <div className="space-y-px">
          {[0, 1, 2].map((row) => (
            <Skeleton key={row} className="h-10 w-full" />
          ))}
        </div>
      )}

      {teams && teams.length === 0 && (
        <EmptyState title="只有一个 Peco 内置团队。" />
      )}

      <ul>
        {(teams ?? []).map((team, index) => (
          <TeamRow
            key={team.team_id}
            team={team}
            skillCount={details[index]?.data?.skills.length ?? null}
          />
        ))}
      </ul>
    </div>
  );
}
