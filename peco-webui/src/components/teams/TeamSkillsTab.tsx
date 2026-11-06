// skills tab：清单 + SKILL.md 只读预览。写入口不在这里 ——
// 技能的编辑要在能改能力根的地方做，团队页只负责「装了什么」。

import { useState } from "react";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { MarkdownRenderer } from "@/components/chat/MarkdownRenderer";
import { useSkillMd, useTeamSkills } from "@/queries/teamDetail";
import { errorText } from "@/lib/api";
import { cn } from "@/lib/utils";

export function TeamSkillsTab({ wsId }: { wsId: string }) {
  const skills = useTeamSkills(wsId);
  const [selected, setSelected] = useState<string | null>(null);
  const rows = skills.data ?? [];
  const active = selected ?? rows[0]?.name ?? null;
  const body = useSkillMd(wsId, active);

  if (skills.error) {
    return (
      <div className="p-3">
        <ErrorBanner
          message={errorText(skills.error)}
          onRetry={() => void skills.refetch()}
        />
      </div>
    );
  }
  if (skills.isPending) {
    return (
      <div className="space-y-px p-3">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-9 w-full" />
        ))}
      </div>
    );
  }
  if (rows.length === 0) {
    return (
      <p className="max-w-[480px] p-3 text-sm text-ink-muted">
        这个团队还没有安装 skill。
      </p>
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col @min-[900px]/main:flex-row">
      <ul className="shrink-0 border-line @min-[900px]/main:w-64 @min-[900px]/main:border-r">
        {rows.map((skill) => (
          <li key={skill.name}>
            <button
              type="button"
              onClick={() => setSelected(skill.name)}
              className={cn(
                "flex h-9 w-full flex-col justify-center border-b border-line px-3 text-left",
                skill.name === active ? "bg-line/40" : "hover:bg-line/20",
              )}
            >
              <span className="truncate font-mono text-sm text-ink">
                {skill.name}
              </span>
              <span className="truncate text-2xs text-ink-faint">
                {skill.description || "没有描述"}
              </span>
            </button>
          </li>
        ))}
      </ul>
      <div className="min-h-0 flex-1 overflow-y-auto p-3">
        {body.error && <ErrorBanner message={errorText(body.error)} />}
        {body.isPending && <Skeleton className="h-16 w-full" />}
        {body.data && <MarkdownRenderer content={body.data.content} />}
      </div>
    </div>
  );
}
