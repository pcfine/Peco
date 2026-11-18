// 新建空间表单：独立页 `/spaces/new` 与团队配置页的「绑定空间」tab 共用。
//
// 独立页在外面补 h1 与说明；嵌进 tab 时由 tab 提供头部，并用 `lockedTeamId`
// 把团队锁成当前团队 —— 在某个团队的配置页里改选别的团队会让人怀疑自己在哪。
//
// 提交成功后由调用方决定去哪：独立页进空间详情，tab 里同样进详情（下一个动作
// 就是看它跑起来）。

import { useMemo, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { createSpace } from "@/api/spaces";
import { createTeam } from "@/api/teams";
import { spaceErrorText } from "@/lib/spaceErrors";
import { useSpaces } from "@/queries/spaces";
import { useTeams } from "@/queries/teams";
import { cn } from "@/lib/utils";

type TeamMode = { kind: "existing"; teamId: string } | { kind: "new" };

interface NewSpaceFormProps {
  /** 预选团队；独立页读 `?team=`，嵌入视图就是当前团队。 */
  initialTeamId?: string;
  /** 给定即锁定团队：隐藏团队选择，空间必然挂在这个团队下。 */
  lockedTeamId?: string;
  onCreated: (wsId: string) => void;
}

export function NewSpaceForm({
  initialTeamId = "",
  lockedTeamId,
  onCreated,
}: NewSpaceFormProps) {
  const queryClient = useQueryClient();
  const spaces = useSpaces();
  const teams = useTeams();

  const [workDir, setWorkDir] = useState("");
  // 「显示名称」写 marker 的 `name`；`label` 属于台账，表单不填（见 spaceLabel）。
  const [name, setName] = useState("");
  const [mode, setMode] = useState<TeamMode>({
    kind: "existing",
    teamId: initialTeamId,
  });
  const [newTeamId, setNewTeamId] = useState("");
  const [newTeamName, setNewTeamName] = useState("");
  const [error, setError] = useState<string | null>(null);

  /** 每团队已有几个空间：多空间共享一个团队是设计特性，标出来而不是隐藏。 */
  const spacesPerTeam = useMemo(() => {
    const map = new Map<string, number>();
    for (const space of spaces.data ?? []) {
      map.set(space.team_id, (map.get(space.team_id) ?? 0) + 1);
    }
    return map;
  }, [spaces.data]);

  const dirOk = workDir.trim().startsWith("/");
  const teamOk = lockedTeamId
    ? true
    : mode.kind === "existing"
      ? mode.teamId !== ""
      : newTeamId.trim() !== "" && newTeamId.trim().length >= 2;

  const submit = useMutation({
    mutationFn: async () => {
      const teamId =
        lockedTeamId ??
        (mode.kind === "existing"
          ? mode.teamId
          : (
              await createTeam({
                id: newTeamId.trim(),
                display_name: newTeamName.trim() || newTeamId.trim(),
                from_template: "team_default",
              })
            ).team_id);
      return createSpace({
        work_dir: workDir.trim(),
        team_id: teamId,
        name: name.trim() || undefined,
      });
    },
    onSuccess: (opened) => {
      toast.success("空间已创建");
      onCreated(opened.ws_id);
    },
    onError: (submitError) => {
      setError(spaceErrorText(submitError));
      // 建团队成功但建空间失败时，团队已经在磁盘上了：让列表刷新出来，
      // 用户改选「现有团队」就能接着走，不必猜自己刚建的 id 还在不在。
      void queryClient.invalidateQueries({ queryKey: ["teams"] });
    },
  });

  return (
    <div className="space-y-6">
      {error && (
        <ErrorBanner message={error} onDismiss={() => setError(null)} />
      )}

      <section className="space-y-3 border-t border-line pt-6">
        <h2 className="text-sm font-semibold text-ink">目录</h2>
        <div className="space-y-1">
          <Label htmlFor="work-dir">工作目录</Label>
          <Input
            id="work-dir"
            value={workDir}
            onChange={(event) => setWorkDir(event.target.value)}
            placeholder="/home/you/work/my-space"
            className="font-mono"
            autoComplete="off"
          />
          <p className="text-caption font-medium text-ink-faint">
            绝对路径。目录不存在会为你创建；已有 `.workspace.json`
            的目录按记录里的身份处理。
          </p>
        </div>
        <div className="space-y-1">
          <Label htmlFor="space-name">显示名称</Label>
          <Input
            id="space-name"
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="例如 客户合同审阅"
          />
        </div>
      </section>

      {!lockedTeamId && (
        <section className="space-y-3 border-t border-line pt-6">
          <h2 className="text-sm font-semibold text-ink">团队</h2>

          {teams.isPending && <Skeleton className="h-9 w-full" />}

          <div className="space-y-1">
            {(teams.data ?? []).map((team) => {
              const count = spacesPerTeam.get(team.team_id) ?? 0;
              const selected =
                mode.kind === "existing" && mode.teamId === team.team_id;
              return (
                <button
                  key={team.team_id}
                  type="button"
                  onClick={() =>
                    setMode({ kind: "existing", teamId: team.team_id })
                  }
                  aria-pressed={selected}
                  className={cn(
                    "flex h-9 w-full items-center gap-2 rounded-md px-2 text-left",
                    selected
                      ? "bg-line text-ink"
                      : "bg-transparent text-ink-muted hover:bg-line/40",
                  )}
                >
                  <span className="min-w-0 flex-1 truncate text-sm">
                    {team.display_name}
                  </span>
                  <span className="shrink-0 font-mono text-2xs text-ink-faint">
                    {team.team_id}
                  </span>
                  <span className="w-28 shrink-0 text-right text-caption font-medium text-ink-faint">
                    已有 {count} 个空间
                  </span>
                </button>
              );
            })}
          </div>

          <button
            type="button"
            onClick={() => setMode({ kind: "new" })}
            aria-pressed={mode.kind === "new"}
            className={cn(
              "flex h-9 w-full items-center rounded-md px-2 text-left text-sm",
              mode.kind === "new"
                ? "bg-line text-ink"
                : "bg-transparent text-ink-muted hover:bg-line/40",
            )}
          >
            由模板新建团队
          </button>

          {mode.kind === "new" && (
            <div className="space-y-2 pl-2">
              <div className="space-y-1">
                <Label htmlFor="team-id">团队 id</Label>
                <Input
                  id="team-id"
                  value={newTeamId}
                  onChange={(event) => setNewTeamId(event.target.value)}
                  placeholder="legal"
                  className="font-mono"
                  autoComplete="off"
                />
                <p className="text-caption font-medium text-ink-faint">
                  小写字母开头，可含数字与连字符，2–32 个字符。
                </p>
              </div>
              <div className="space-y-1">
                <Label htmlFor="team-name">显示名</Label>
                <Input
                  id="team-name"
                  value={newTeamName}
                  onChange={(event) => setNewTeamName(event.target.value)}
                  placeholder="法务"
                />
              </div>
            </div>
          )}
        </section>
      )}

      <div className="flex items-center justify-end gap-2 border-t border-line pt-4">
        {!dirOk && workDir !== "" && (
          <span className="mr-auto text-caption font-medium text-warn">
            目录要写绝对路径
          </span>
        )}
        <Button
          disabled={!dirOk || !teamOk || submit.isPending}
          onClick={() => {
            setError(null);
            submit.mutate();
          }}
        >
          创建并打开
        </Button>
      </div>
    </div>
  );
}
