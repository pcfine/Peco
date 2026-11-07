// `/settings` —— 团队级配置入口。团队就是配置边界（providers.toml 在团队能力根
// 下），所以页面第一件事是选团队，默认跟着「当前空间」所属团队走。

import { useState } from "react";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { ProviderSection } from "@/components/settings/ProviderSection";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { errorText } from "@/lib/api";
import { useSpaces } from "@/queries/spaces";
import { useTeams } from "@/queries/teams";
import { useSpaceStore } from "@/stores/spaceStore";

export function SettingsPage() {
  const teams = useTeams();
  const spaces = useSpaces();
  const currentWsId = useSpaceStore((state) => state.currentWsId);
  const [pickedTeamId, setPickedTeamId] = useState<string | null>(null);

  const rows = teams.data ?? [];
  const currentTeamId =
    (spaces.data ?? []).find((space) => space.ws_id === currentWsId)?.team_id ??
    null;
  const existingTeamId = (teamId: string | null): string | null =>
    teamId !== null && rows.some((team) => team.team_id === teamId)
      ? teamId
      : null;
  // 选中项可能已经被删掉：回落到「当前空间的团队」，再回落到第一个团队。
  const teamId =
    existingTeamId(pickedTeamId) ??
    existingTeamId(currentTeamId) ??
    rows[0]?.team_id ??
    null;
  const active = rows.find((team) => team.team_id === teamId) ?? null;

  return (
    <div className="max-w-[880px] space-y-6">
      <section className="space-y-2">
        <div className="flex h-8 items-center gap-2 border-b border-line">
          <h2 className="text-xs font-semibold text-ink-muted">团队</h2>
          <span className="truncate text-2xs text-ink-faint">
            provider 配置存在团队能力根里，切换团队就切换配置
          </span>
        </div>

        {teams.error !== null && (
          <ErrorBanner
            message={errorText(teams.error)}
            onRetry={() => void teams.refetch()}
          />
        )}

        {teams.isPending && <Skeleton className="h-8 w-56" />}

        {teams.data && rows.length === 0 && (
          <EmptyState title="还没有团队，provider 配置没有落点。" />
        )}

        {active && (
          <div className="flex h-9 items-center gap-2">
            {rows.length === 1 ? (
              <span className="text-sm text-ink">{active.display_name}</span>
            ) : (
              <Select value={active.team_id} onValueChange={setPickedTeamId}>
                <SelectTrigger className="h-8 w-56" aria-label="团队">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {rows.map((team) => (
                    <SelectItem key={team.team_id} value={team.team_id}>
                      {team.display_name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )}
            <span className="font-mono text-mono-sm text-ink-faint">
              {active.team_id}
            </span>
          </div>
        )}
      </section>

      {teamId !== null && <ProviderSection teamId={teamId} />}
    </div>
  );
}
