// `/teams` 团队列表。

import { useEffect, useRef, useState, type ChangeEvent } from "react";
import { Link, useNavigate } from "react-router-dom";
import { Camera, Loader2, Plus, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { PixelIdenticon } from "@/components/common/PixelIdenticon";
import { PageFrame } from "@/components/layout/PageFrame";
import { DeleteTeamDialog } from "@/components/teams/DeleteTeamDialog";
import { NewTeamDialog } from "@/components/teams/NewTeamDialog";
import { uploadImage } from "@/api/upload";
import { errorText } from "@/lib/api";
import { createIdenticon, resolveIcon } from "@/lib/identicon";
import { resizeImage } from "@/lib/image";
import { useDeleteTeam, useTeams, useUpdateTeamIcon } from "@/queries/teams";
import { cn } from "@/lib/utils";
import type { Team } from "@/types/daemon";

/** 后端未给出计数时的占位：`0` 是事实，「–」只是「还没告诉我」。 */
const MISSING = "–";

/** 卡片固定高度；列数按 300px 基准均分容器宽度。 */
const GRID = "grid grid-cols-[repeat(auto-fill,minmax(300px,1fr))] gap-2.5";

const STATS = [
  { key: "agent", label: "agents", of: (team: Team) => team.agent_count },
  { key: "skill", label: "skills", of: (team: Team) => team.skill_count },
  { key: "mcp", label: "MCP", of: (team: Team) => team.mcp_count },
  { key: "space", label: "空间", of: (team: Team) => team.space_count },
] as const;

function TeamCard({ team, onDelete }: { team: Team; onDelete: () => void }) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [uploading, setUploading] = useState(false);
  const [imageBroken, setImageBroken] = useState(false);
  const updateIcon = useUpdateTeamIcon();
  const icon = resolveIcon(team.icon, team.team_id);
  const showImage = icon.kind === "image" && !imageBroken;

  useEffect(() => setImageBroken(false), [team.icon]);

  async function onPick(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;

    setUploading(true);
    try {
      const blob = await resizeImage(file, 256);
      const url = await uploadImage(blob);
      await updateIcon.mutateAsync({ teamId: team.team_id, icon: url });
    } catch (error) {
      toast.error(errorText(error));
    } finally {
      setUploading(false);
    }
  }

  return (
    <li
      data-team-id={team.team_id}
      className="group relative h-[115px] min-w-0 overflow-hidden rounded-xl border border-line bg-panel transition-colors group-hover:border-brand/60 group-focus-within:border-brand/60"
    >
      <Link
        to={`/teams/${encodeURIComponent(team.team_id)}`}
        aria-label={`打开 ${team.display_name}`}
        className="absolute inset-0 z-0 rounded-xl"
      />

      <div className="pointer-events-none relative z-10 flex h-full gap-2 p-2">
        <div data-avatar className="relative aspect-square h-full shrink-0">
          <div className="h-full w-full overflow-hidden rounded-lg border border-line/60 bg-raised">
            {showImage ? (
              <img
                src={icon.value}
                alt=""
                className="h-full w-full object-cover"
                onError={() => setImageBroken(true)}
              />
            ) : icon.kind === "emoji" ? (
              <span className="flex h-full w-full items-center justify-center text-2xl select-none">
                {icon.value}
              </span>
            ) : (
              <PixelIdenticon
                identicon={icon.identicon ?? createIdenticon(team.team_id)}
                className="h-full w-full"
              />
            )}
          </div>

          <button
            type="button"
            disabled={uploading}
            aria-label={
              uploading
                ? `正在上传 ${team.display_name} 的团队图标`
                : `更换 ${team.display_name} 的团队图标`
            }
            onClick={() => inputRef.current?.click()}
            className={cn(
              "pointer-events-auto absolute -right-1.5 -bottom-1.5 grid size-6 place-items-center rounded-full border border-line bg-panel text-ink-muted transition-opacity hover:text-ink",
              "opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 focus-visible:opacity-100 pointer-coarse:opacity-100",
              "disabled:cursor-wait",
            )}
          >
            {uploading ? (
              <Loader2 className="size-3.5 animate-spin" />
            ) : (
              <Camera className="size-3.5" />
            )}
          </button>
        </div>

        <div className="flex min-w-0 flex-1 flex-col py-0.5">
          <div className="flex items-start gap-2">
            <span className="min-w-0 flex-1 truncate text-base font-semibold text-ink">
              {team.display_name}
            </span>
            <div className="pointer-events-auto -mt-1 -mr-1 flex shrink-0 opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100 pointer-coarse:opacity-100">
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                aria-label={`删除 ${team.display_name}`}
                title="删除团队"
                className="text-ink-muted hover:text-destructive"
                onClick={onDelete}
              >
                <Trash2 className="size-3" />
              </Button>
            </div>
          </div>
          <span className="truncate font-mono text-2xs text-ink-faint">
            {team.team_id}
          </span>
          <div className="mt-auto grid grid-cols-2 gap-x-3 gap-y-0 text-caption font-medium">
            {STATS.map((stat) => (
              <span key={stat.key} className="flex items-baseline gap-1">
                <span className="text-ink-faint">{stat.label}</span>
                <span data-stat={stat.key} className="tabular-nums text-ink">
                  {stat.of(team) ?? MISSING}
                </span>
              </span>
            ))}
          </div>
        </div>
      </div>

      <input
        ref={inputRef}
        type="file"
        accept="image/png,image/jpeg,image/gif,image/webp"
        tabIndex={-1}
        className="sr-only"
        onChange={(event) => void onPick(event)}
      />
    </li>
  );
}

export function TeamListPage() {
  const navigate = useNavigate();
  const { data: teams, error, isPending, refetch } = useTeams();
  const remove = useDeleteTeam();
  const [newOpen, setNewOpen] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<Team | null>(null);

  function requestDelete(team: Team) {
    if (team.bound_spaces.length === 0) {
      remove.mutate(team.team_id, {
        onError: (mutationError) => toast.error(errorText(mutationError)),
      });
      return;
    }
    setPendingDelete(team);
  }

  return (
    <PageFrame
      title="团队"
      actions={
        <Button size="sm" onClick={() => setNewOpen(true)}>
          <Plus className="size-4" />
          新建团队
        </Button>
      }
    >
      <div className="space-y-4">
        {error && (
          <ErrorBanner
            message={errorText(error)}
            onRetry={() => void refetch()}
          />
        )}

        {isPending && (
          <div className={GRID}>
            {[0, 1, 2, 3, 4, 5].map((card) => (
              <Skeleton key={card} className="h-[115px] w-full rounded-xl" />
            ))}
          </div>
        )}

        <ul className={GRID}>
          {(teams ?? []).map((team) => (
            <TeamCard
              key={team.team_id}
              team={team}
              onDelete={() => requestDelete(team)}
            />
          ))}
        </ul>

        <NewTeamDialog
          open={newOpen}
          onOpenChange={setNewOpen}
          teams={teams ?? []}
          onCreated={(team) => {
            setNewOpen(false);
            navigate(`/teams/${encodeURIComponent(team.team_id)}`);
          }}
        />

        <DeleteTeamDialog
          team={pendingDelete}
          onOpenChange={(open) => {
            if (!open) setPendingDelete(null);
          }}
          onDeleted={() => setPendingDelete(null)}
        />
      </div>
    </PageFrame>
  );
}
