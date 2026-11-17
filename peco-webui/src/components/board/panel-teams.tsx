import { Link } from "react-router-dom";
import { PanelShell } from "@/components/board/PanelShell";
import { EmptyState } from "@/components/common/EmptyState";
import { Button } from "@/components/ui/button";
import { useTeams } from "@/queries/teams";

const VISIBLE_ROWS = 4;

export function PanelTeams() {
  const { data: teams, error, isPending, isFetching, refetch } = useTeams();
  const rows = (teams ?? []).slice(0, VISIBLE_ROWS);

  return (
    <PanelShell
      title="团队概览"
      isPending={isPending}
      isFetching={isFetching}
      error={error}
      hasData={Boolean(teams)}
      onRetry={() => void refetch()}
    >
      {teams && teams.length === 0 ? (
        <EmptyState
          title="只有一个 Peco 内置团队"
          action={
            <Button asChild variant="ghost" size="sm">
              <Link to="/teams">查看团队</Link>
            </Button>
          }
        />
      ) : (
        <ul className="divide-y divide-line">
          {rows.map((team) => (
            <li key={team.team_id} className="flex h-8 items-center gap-2">
              <span className="min-w-0 flex-1 truncate text-sm text-ink">
                {team.display_name}
              </span>
              <span className="shrink-0 tabular-nums text-caption font-medium text-ink-faint @min-[640px]/work:w-[72px] @min-[640px]/work:text-right">
                空间 {team.bound_spaces.length}
              </span>
              {/* 窄档隐藏 agents 列：两列已经够读，第三列会把团队名压没。 */}
              <span className="hidden shrink-0 tabular-nums text-caption font-medium text-ink-faint @min-[640px]/work:inline @min-[640px]/work:w-16 @min-[640px]/work:text-right">
                agents {team.agents.length}
              </span>
            </li>
          ))}
        </ul>
      )}
      {(teams ?? []).length > 0 && (
        <Link
          to="/teams"
          className="tap-target mt-1 -mx-2 px-2 text-caption font-medium text-ink-faint hover:text-ink"
        >
          全部团队
        </Link>
      )}
    </PanelShell>
  );
}
