// 绑定空间 tab：团队是能力边界，空间是执行实例 —— 一个团队可以有多个空间，
// 这是设计特性，所以这里不做「只能有一个」的暗示。
//
// 「新建空间」在同一块内容区里换成表单，左侧分类导航始终可见。
// 表单与 `/spaces/new` 共用 `NewSpaceForm`，团队锁成当前团队。

import { useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { EmptyState } from "@/components/common/EmptyState";
import { NewSpaceForm } from "@/components/spaces/NewSpaceForm";
import { Button } from "@/components/ui/button";
import { relativeTime } from "@/lib/relative";
import type { TeamSpace } from "@/types/daemon";

export function TeamSpacesTab({
  teamId,
  spaces,
}: {
  teamId: string;
  spaces: TeamSpace[];
}) {
  const navigate = useNavigate();
  const [creating, setCreating] = useState(false);

  if (creating) {
    return (
      <div className="flex h-full min-h-0 flex-col">
        <div className="flex h-9 shrink-0 items-center gap-2 border-b border-line px-2">
          <Button variant="ghost" size="sm" onClick={() => setCreating(false)}>
            返回列表
          </Button>
          <span className="text-sm font-medium text-ink">新建空间</span>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto p-3">
          <NewSpaceForm
            lockedTeamId={teamId}
            onCreated={(wsId) =>
              navigate(`/spaces/${encodeURIComponent(wsId)}`)
            }
          />
        </div>
      </div>
    );
  }

  return (
    <div className="p-3">
      <div className="mb-3 flex items-center gap-2">
        <Button size="sm" onClick={() => setCreating(true)}>
          ＋ 新建空间
        </Button>
        <span className="ml-auto flex items-center gap-2">
          <span className="text-sm font-medium text-ink">绑定空间</span>
          <span className="text-caption text-ink-faint">{spaces.length}</span>
        </span>
      </div>

      {spaces.length === 0 ? (
        <EmptyState title="这个团队还没有绑定空间" />
      ) : (
        <ul>
          {spaces.map((space) => (
            <li key={space.ws_id} className="border-b border-line">
              <Link
                to={`/spaces/${encodeURIComponent(space.ws_id)}`}
                className="flex h-9 items-center gap-2 px-3 hover:bg-line/30"
              >
                <span className="min-w-0 flex-1 truncate font-mono text-2xs text-ink">
                  {space.work_dir}
                </span>
                <span className="shrink-0 text-caption font-medium text-ink-faint">
                  {space.closed_at ? "已关闭" : "进行中"}
                </span>
                <span className="w-20 shrink-0 text-right text-caption font-medium text-ink-faint">
                  {relativeTime(space.closed_at)}
                </span>
              </Link>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
