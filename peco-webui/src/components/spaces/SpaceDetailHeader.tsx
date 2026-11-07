// 空间详情头部 h-76：名称行 32 + 元信息行 20 + 内边距 12/12。
// 动作里「关闭」始终独立可见 —— 破坏性操作不折进 ⋯ 菜单。

import { Link } from "react-router-dom";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import {
  SpaceStatusMark,
  spaceGroupOf,
} from "@/components/layout/SpaceStatusMark";
import { spaceLabel } from "@/components/layout/spaceLabel";
import { useOpenSpace } from "@/components/spaces/useSpaceLifecycle";
import { useSpaceStore } from "@/stores/spaceStore";
import { relativeTime } from "@/lib/relative";
import type { Space } from "@/types/daemon";

interface SpaceDetailHeaderProps {
  space: Space;
  onClose: () => void;
  closing: boolean;
  /** 右侧附加动作（聊天页的右栏模式按钮组）。开关按钮始终在最右。 */
  actions?: React.ReactNode;
}

export function SpaceDetailHeader({
  space,
  onClose,
  closing,
  actions,
}: SpaceDetailHeaderProps) {
  const { open, opening } = useOpenSpace(
    space.ws_id,
    space.work_dir,
    space.team_id,
  );
  const currentWsId = useSpaceStore((state) => state.currentWsId);
  const setCurrent = useSpaceStore((state) => state.setCurrent);
  const isCurrent = currentWsId === space.ws_id;
  const name = spaceLabel(space);

  const copyWorkDir = async () => {
    try {
      await navigator.clipboard.writeText(space.work_dir);
      toast.success("目录已复制");
    } catch {
      // 剪贴板要 https 或用户授权；拿不到时把路径本身留给用户手选。
      toast.error("复制失败，请手动选择目录文本");
    }
  };

  return (
    <header className="shrink-0 border-b border-line px-3 py-3">
      <div className="flex h-8 items-center gap-2">
        <h1 className="truncate text-xl font-semibold text-ink">{name}</h1>
        <SpaceStatusMark group={spaceGroupOf(space)} />
        <span className="shrink-0 text-2xs text-ink-faint">
          {spaceGroupOf(space) === "running" ? "进行中" : "未打开"}
        </span>
        {!isCurrent && (
          <button
            type="button"
            onClick={() => setCurrent(space.ws_id)}
            className="shrink-0 rounded-[4px] border border-line px-1.5 text-2xs text-ink-muted hover:text-ink"
          >
            非当前空间
          </button>
        )}
        <div className="ml-auto flex shrink-0 items-center gap-1">
          {actions}
          {space.status === "closed" && (
            <Button
              variant="outline"
              size="sm"
              className="h-7"
              disabled={opening}
              onClick={open}
            >
              打开
            </Button>
          )}
          <Button
            variant="outline"
            size="sm"
            className="h-7 border-danger/40 text-danger"
            disabled={closing || space.status === "closed"}
            onClick={onClose}
          >
            关闭
          </Button>
        </div>
      </div>

      <div className="mt-1 flex h-5 min-w-0 items-center gap-2 text-2xs text-ink-faint">
        <span
          className="truncate font-mono text-mono-sm"
          title={space.work_dir}
        >
          {space.work_dir}
        </span>
        <button
          type="button"
          onClick={() => void copyWorkDir()}
          className="shrink-0 text-2xs text-ink-muted hover:text-ink"
        >
          复制
        </button>
        <span className="shrink-0">·</span>
        <Link
          to={`/teams/${encodeURIComponent(space.team_id)}`}
          className="shrink-0 text-ink-muted hover:text-ink"
        >
          团队 {space.team_id}
        </Link>
        <span className="shrink-0">·</span>
        <span className="shrink-0">退出策略 {space.effective_exit_policy}</span>
        <span className="shrink-0">·</span>
        <span className="shrink-0 tabular-nums">
          在飞 {space.inflight_dispatch + space.inflight_foreign}
        </span>
        <span className="shrink-0">·</span>
        <span className="shrink-0">
          最近活动 {relativeTime(space.last_opened_at)}
        </span>
      </div>

      {!isCurrent && (
        <div className="mt-1 flex h-5 items-center gap-2 text-2xs text-ink-muted">
          <span className="truncate">正在查看 {name}，非当前空间</span>
          <button
            type="button"
            onClick={() => setCurrent(space.ws_id)}
            className="shrink-0 text-primary-ink hover:underline"
          >
            设为当前
          </button>
        </div>
      )}
    </header>
  );
}
