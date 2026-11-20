// `/settings` —— 配置入口。provider 配置落在两个范围上：系统层（本机一份，所有
// 团队继承）与团队层（团队能力根）。页面第一件事因此是选「配置范围」，默认跟着
// 「当前空间」所属团队走；零团队时系统层是唯一可配的东西。
//
// 下拉里还有保留团队 Peco：它不在默认团队清单里，却是团队层的默认兜底。

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { PageFrame } from "@/components/layout/PageFrame";
import { AuthSection } from "@/components/settings/AuthSection";
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
import {
  SYSTEM_SCOPE_KEY,
  listProvidersFor,
  providersQueryKey,
  scopeKey,
} from "@/api/providers";
import type { ProviderInfo, ProviderScope } from "@/api/providers";
import { errorText } from "@/lib/api";
import { useSpaces } from "@/queries/spaces";
import { useTeamsWithPeco } from "@/queries/teams";
import { useSpaceStore } from "@/stores/spaceStore";
import type { Team } from "@/types/daemon";

/** 下拉里置顶的伪条目：它不是团队，是继承链的终点。 */
const SYSTEM_SCOPE_LABEL = "系统（本机）";

const PECO_UNCONFIGURED =
  "Peco 和系统层都没有一条能用的 provider 密钥 —— 没配 provider 的团队会一路落空，对话直接报错。请先在『系统（本机）』填一条（其它团队都会继承它），或在 Peco 里配一条。";

/**
 * 继承链上一条可解析的密钥都没有。
 *
 * 链的终点是系统层文件，Peco 不再是终点：任一层有能解析出密钥的条目就够用。
 * `has_api_key` 已含解析语义，直接复用它。
 */
export function pecoNeedsSetup(providers: ProviderInfo[] | undefined): boolean {
  if (!providers) return false;
  return !providers.some((provider) => provider.has_api_key);
}

/** Peco 置顶：它是团队层的兜底，先看见它才知道别的团队在继承什么。 */
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
  const [pickedScope, setPickedScope] = useState<ProviderScope | null>(null);

  const rows = orderTeams(teams.data ?? []);
  const currentTeamId =
    (spaces.data ?? []).find((space) => space.ws_id === currentWsId)?.team_id ??
    null;
  /** 只作用于 team 分支：伪条目不是团队，不能被 rows 过滤掉。 */
  const existingTeamId = (teamId: string | null): string | null =>
    teamId !== null && rows.some((team) => team.team_id === teamId)
      ? teamId
      : null;
  // 选中项可能已经被删掉：回落到「当前空间的团队」，再回落到第一个团队。
  const pickedTeamId =
    pickedScope?.kind === "team" ? existingTeamId(pickedScope.teamId) : null;
  const teamId =
    pickedTeamId ?? existingTeamId(currentTeamId) ?? rows[0]?.team_id ?? null;
  const scope: ProviderScope =
    pickedScope?.kind === "system"
      ? pickedScope
      : teamId !== null
        ? { kind: "team", teamId }
        : { kind: "system" };

  // 只在这里提示，不拦任何保存 —— 别的团队可能本来就靠系统层继承。
  const pecoTeamId = rows.find((team) => team.reserved)?.team_id ?? null;
  const pecoProviders = useQuery({
    queryKey: providersQueryKey({ kind: "team", teamId: pecoTeamId ?? "" }),
    queryFn: () =>
      listProvidersFor({ kind: "team", teamId: pecoTeamId as string }),
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
            <h2 className="text-xs font-semibold text-ink-muted">配置范围</h2>
            <span className="min-w-0 truncate text-caption font-medium text-ink-faint">
              {scope.kind === "system"
                ? "本机一份，所有团队都继承；改它会立刻影响全部团队"
                : scope.teamId === pecoTeamId
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
            <EmptyState title="还没有团队；先在这里配系统 provider，团队建好后自动继承" />
          )}

          <div className="flex h-9 items-center gap-2">
            {rows.length >= 1 ? (
              <Select
                value={scopeKey(scope)}
                onValueChange={(value) =>
                  setPickedScope(
                    value === SYSTEM_SCOPE_KEY
                      ? { kind: "system" }
                      : { kind: "team", teamId: value },
                  )
                }
              >
                <SelectTrigger className="h-8 w-56" aria-label="配置范围">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={SYSTEM_SCOPE_KEY}>
                    {SYSTEM_SCOPE_LABEL}
                  </SelectItem>
                  {rows.map((team) => (
                    <SelectItem key={team.team_id} value={team.team_id}>
                      {teamLabel(team)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : (
              <span className="text-sm text-ink">{SYSTEM_SCOPE_LABEL}</span>
            )}
            {scope.kind === "team" && (
              <span className="font-mono text-mono-sm text-ink-faint">
                {scope.teamId}
              </span>
            )}
          </div>

          {pecoUnconfigured && (
            <p className="border-l-2 border-warn-mark pl-2 text-xs text-warn">
              {PECO_UNCONFIGURED}
            </p>
          )}
        </section>

        <ProviderSection scope={scope} />

        <AuthSection />
      </div>
    </PageFrame>
  );
}
