// `/settings` —— 团队级配置入口。团队就是配置边界（providers.toml 在团队能力根
// 下），所以页面第一件事是选团队，默认跟着「当前空间」所属团队走。
//
// 下拉里还有保留团队 Peco：它不在默认团队清单里，却是 provider 继承链的默认层，
// 其它团队没配的条目都从它落到系统层。

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { PageFrame } from "@/components/layout/PageFrame";
import { BrandSection } from "@/components/settings/BrandSection";
import { ProviderSection } from "@/components/settings/ProviderSection";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { listProviders, providerSource } from "@/api/providers";
import type { ProviderInfo } from "@/api/providers";
import { errorText } from "@/lib/api";
import { useSpaces } from "@/queries/spaces";
import { useTeamsWithPeco } from "@/queries/teams";
import { useSpaceStore } from "@/stores/spaceStore";
import type { Team } from "@/types/daemon";

const PECO_UNCONFIGURED =
  "Peco 还没有配置自己的 provider 密钥，没配 provider 的团队会直接落到系统层的环境变量。";

/**
 * Peco 一个带密钥的自有条目都没有 —— 继承链到这里就断了，下面直接是系统环境变量。
 *
 * 只看 `team` 层：从系统层继承来的条目不是 Peco 的配置，拿它当「已配置」会把这条
 * 提示永远藏起来。
 */
export function pecoNeedsSetup(providers: ProviderInfo[] | undefined): boolean {
  if (!providers) return false;
  return !providers.some(
    (provider) => providerSource(provider) === "team" && provider.has_api_key,
  );
}

/** Peco 置顶：它是兜底层，先看见它才知道别的团队在继承什么。 */
export function orderTeams(teams: Team[]): Team[] {
  return [...teams].sort(
    (a, b) => Number(b.reserved ?? false) - Number(a.reserved ?? false),
  );
}

/** 下拉与标题里显示的名字；Peco 要自报身份，否则和普通团队长得一样。 */
export function teamLabel(team: Team): string {
  return team.reserved ? `${team.display_name}（默认兜底）` : team.display_name;
}

export function SettingsPage() {
  const teams = useTeamsWithPeco();
  const spaces = useSpaces();
  const currentWsId = useSpaceStore((state) => state.currentWsId);
  const [pickedTeamId, setPickedTeamId] = useState<string | null>(null);

  const rows = orderTeams(teams.data ?? []);
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

  // Peco 自己的密钥是整条继承链的终点：没了它，没配 provider 的团队会直接落到
  // 系统环境变量。只在这里提示，不拦任何保存 —— 别的团队可能本来就靠环境变量。
  const pecoTeamId = rows.find((team) => team.reserved)?.team_id ?? null;
  const pecoProviders = useQuery({
    queryKey: ["providers", pecoTeamId],
    queryFn: () => listProviders(pecoTeamId as string),
    enabled: pecoTeamId !== null,
  });
  const pecoUnconfigured =
    pecoTeamId !== null && pecoNeedsSetup(pecoProviders.data);

  return (
    <PageFrame title="设置" contentWidth="mid">
      <div className="space-y-6">
        <BrandSection />

        <section className="space-y-2">
          <div className="flex h-8 items-center gap-2 border-b border-line">
            <h2 className="text-xs font-semibold text-ink-muted">团队</h2>
            <span className="min-w-0 truncate text-caption font-medium text-ink-faint">
              {active?.reserved
                ? "其它团队未配置的 provider 从这里继承"
                : "provider 配置存在团队能力根里，切换团队就切换配置"}
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
            <EmptyState title="还没有团队，provider 配置没有落点" />
          )}

          {active && (
            <div className="flex h-9 items-center gap-2">
              {rows.length === 1 ? (
                <span className="text-sm text-ink">{teamLabel(active)}</span>
              ) : (
                <Select value={active.team_id} onValueChange={setPickedTeamId}>
                  <SelectTrigger className="h-8 w-56" aria-label="团队">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {rows.map((team) => (
                      <SelectItem key={team.team_id} value={team.team_id}>
                        {teamLabel(team)}
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

          {pecoUnconfigured && (
            <p className="border-l-2 border-warn-mark pl-2 text-xs text-warn">
              {PECO_UNCONFIGURED}
            </p>
          )}
        </section>

        {teamId !== null && <ProviderSection teamId={teamId} />}
      </div>
    </PageFrame>
  );
}
