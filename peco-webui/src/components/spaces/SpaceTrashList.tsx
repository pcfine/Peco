// 回收站：只放已经从清单里消失的空间。每行两条路 —— 恢复（可撤销的后悔药）与
// 永久删除（会话与记忆一起没），后者必须再确认一次。

import { useState } from "react";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { spaceLabel } from "@/components/layout/spaceLabel";
import { SpaceRemovalDialog } from "@/components/spaces/SpaceRemovalDialog";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { errorText } from "@/lib/api";
import { relativeTime } from "@/lib/relative";
import {
  usePurgeSpace,
  useRestoreSpace,
  useTrashSpaces,
} from "@/queries/spaces";
import type { TrashSpace } from "@/types/daemon";

/** 回收站行只有 `name`，没有 `label`：显示名回落链与清单里同一个函数决定。 */
function trashLabel(row: TrashSpace): string {
  return spaceLabel({ name: row.name, label: null, work_dir: row.work_dir });
}

function TrashRow({
  row,
  restoring,
  onRestore,
  onPurge,
}: {
  row: TrashSpace;
  restoring: boolean;
  onRestore: () => void;
  onPurge: () => void;
}) {
  const name = trashLabel(row);
  return (
    <li
      data-ws-id={row.ws_id}
      className="flex h-10 items-center gap-2 border-b border-line px-3"
    >
      <span className="min-w-0 flex-1 truncate text-sm text-ink">{name}</span>
      <span className="hidden w-[120px] shrink-0 truncate font-mono text-2xs text-ink-faint @min-[640px]/work:block">
        {row.team_id}
      </span>
      <span className="shrink-0 tabular-nums text-caption font-medium text-ink-faint">
        移入 {relativeTime(row.trashed_at)}
      </span>
      <Button
        variant="ghost"
        size="xs"
        aria-label={`恢复空间 ${name}`}
        disabled={restoring}
        onClick={onRestore}
      >
        恢复
      </Button>
      <Button
        variant="ghost"
        size="xs"
        className="text-danger hover:text-danger"
        aria-label={`永久删除空间 ${name}`}
        disabled={restoring}
        onClick={onPurge}
      >
        永久删除
      </Button>
    </li>
  );
}

export function SpaceTrashList() {
  const { data, error, isPending, refetch } = useTrashSpaces();
  const restore = useRestoreSpace();
  const purge = usePurgeSpace();
  const [purging, setPurging] = useState<TrashSpace | null>(null);

  const rows = data ?? [];

  return (
    <div className="space-y-3">
      {error && (
        <ErrorBanner
          message={errorText(error)}
          onRetry={() => void refetch()}
        />
      )}

      {isPending && (
        <div className="space-y-px">
          {[0, 1, 2].map((row) => (
            <Skeleton key={row} className="h-10 w-full" />
          ))}
        </div>
      )}

      {!isPending && !error && rows.length === 0 && (
        <EmptyState title="回收站是空的：移入回收站的空间会列在这里" />
      )}

      {rows.length > 0 && (
        <ul className="min-w-0 overflow-x-auto">
          {rows.map((row) => (
            <TrashRow
              key={row.ws_id}
              row={row}
              restoring={restore.isPending && restore.variables === row.ws_id}
              onRestore={() => restore.mutate(row.ws_id)}
              onPurge={() => setPurging(row)}
            />
          ))}
        </ul>
      )}

      {purging && (
        <SpaceRemovalDialog
          name={trashLabel(purging)}
          mode="purge"
          run={(force) =>
            purge.mutate(
              { wsId: purging.ws_id, force },
              { onSuccess: () => setPurging(null) },
            )
          }
          pending={purge.isPending}
          error={purge.error}
          onClose={() => {
            setPurging(null);
            purge.reset();
          }}
        />
      )}
    </div>
  );
}
