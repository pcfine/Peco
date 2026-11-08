// `/spaces/new` —— 两块同时可见，不用步骤条、不加序号（序号是装饰）。
// 提交成功直接进空间详情：向导的下一个动作就是「看它跑起来」。

import { useMemo, useState } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
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

export function NewSpacePage() {
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const queryClient = useQueryClient();
  const spaces = useSpaces();
  const teams = useTeams();

  const [workDir, setWorkDir] = useState("");
  const [label, setLabel] = useState("");
  const [mode, setMode] = useState<TeamMode>({
    kind: "existing",
    teamId: searchParams.get("team") ?? "",
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
  const teamOk =
    mode.kind === "existing"
      ? mode.teamId !== ""
      : newTeamId.trim() !== "" && newTeamId.trim().length >= 2;

  const submit = useMutation({
    mutationFn: async () => {
      const teamId =
        mode.kind === "existing"
          ? mode.teamId
          : (
              await createTeam({
                id: newTeamId.trim(),
                display_name: newTeamName.trim() || newTeamId.trim(),
                from_template: "team_default",
              })
            ).team_id;
      return createSpace({
        work_dir: workDir.trim(),
        team_id: teamId,
        label: label.trim() || undefined,
      });
    },
    onSuccess: (opened) => {
      toast.success("空间已创建");
      navigate(`/spaces/${encodeURIComponent(opened.ws_id)}`);
    },
    onError: (submitError) => {
      setError(spaceErrorText(submitError));
      // 建团队成功但建空间失败时，团队已经在磁盘上了：让列表刷新出来，
      // 用户改选「现有团队」就能接着走，不必猜自己刚建的 id 还在不在。
      void queryClient.invalidateQueries({ queryKey: ["teams"] });
    },
  });

  return (
    <div className="max-w-[720px] space-y-6">
      <h1 className="text-xl font-semibold text-ink">新建空间</h1>
      <p className="max-w-[640px] text-sm text-ink-muted">
        空间 = 一个目录 + 一个团队。目录里的 `.workspace.json` 记录它的身份，
        团队决定它能用哪些 agent 与 skill。
      </p>

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
            value={label}
            onChange={(event) => setLabel(event.target.value)}
            placeholder="例如 客户合同审阅"
          />
        </div>
      </section>

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
                  "flex h-9 w-full items-center gap-2 rounded-[6px] px-2 text-left",
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
            "flex h-9 w-full items-center rounded-[6px] px-2 text-left text-sm",
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
