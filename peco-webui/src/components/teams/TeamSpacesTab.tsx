// 绑定空间 tab：团队是能力边界，空间是执行实例 —— 一个团队可以有多个空间，
// 这是设计特性，所以这里不做「只能有一个」的暗示。

import { Link } from "react-router-dom";
import { EmptyState } from "@/components/common/EmptyState";
import { relativeTime } from "@/lib/relative";
import type { TeamSpace } from "@/types/daemon";

export function TeamSpacesTab({ spaces }: { spaces: TeamSpace[] }) {
  if (spaces.length === 0) {
    return (
      <div className="p-3">
        <EmptyState
          title="这个团队还没有绑定空间。"
          action={
            <Link
              to="/spaces/new"
              className="text-xs text-primary-ink hover:underline"
            >
              新建空间
            </Link>
          }
        />
      </div>
    );
  }

  return (
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
  );
}
