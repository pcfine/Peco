// `/spaces/:wsId` —— 本阶段是页头 + 聊天占位：单对话壳与右栏在下一阶段接入。
// 空间的开关、台账、产物、文件都已搬到 `/spaces/:wsId/status`，这里不重复承载。

import { Link, useParams } from "react-router-dom";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { CloseSpaceDialog } from "@/components/spaces/CloseSpaceDialog";
import { SpaceDetailHeader } from "@/components/spaces/SpaceDetailHeader";
import { useCloseSpace } from "@/components/spaces/useSpaceLifecycle";
import { useSpace } from "@/queries/spaceDetail";
import { errorText } from "@/lib/api";

export function SpaceDetailPage() {
  const { wsId = "" } = useParams();
  const space = useSpace(wsId);
  const close = useCloseSpace(wsId);

  if (space.isPending) {
    return (
      <div className="space-y-px">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-10 w-full" />
        ))}
      </div>
    );
  }

  if (space.error || !space.data) {
    return (
      <div className="space-y-3">
        <ErrorBanner
          message={errorText(space.error)}
          onRetry={() => void space.refetch()}
        />
        <Link to="/spaces" className="text-xs text-primary-ink hover:underline">
          返回工作室
        </Link>
      </div>
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <SpaceDetailHeader
        space={space.data}
        onClose={close.close}
        closing={close.closing}
      />

      <div className="min-h-0 flex-1 overflow-y-auto p-3">
        <div className="max-w-[480px] space-y-2">
          <p className="text-sm text-ink-muted">
            这个空间只有一条 @master 对话，聊天区下一阶段接到这里。
          </p>
          <Link
            to={`/spaces/${encodeURIComponent(wsId)}/status`}
            className="text-xs text-primary-ink hover:underline"
          >
            查看空间状态
          </Link>
        </div>
      </div>

      <CloseSpaceDialog
        open={close.pending !== null}
        inflight={close.pending}
        onWait={close.dismiss}
        onForce={() => close.forceClose()}
        forcing={close.forcing}
        error={close.error}
      />
    </div>
  );
}
