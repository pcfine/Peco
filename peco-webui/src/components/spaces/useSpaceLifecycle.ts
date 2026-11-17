// 空间的开关动作。关闭被 409 拦下时，两个在飞计数直接来自那一刻的响应体，
// 不从空间详情里另取一份 —— 详情里的是 15s 前的数，而挡住决定的就是这一刻的数。

import { useEffect, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { closeSpace, createSpace, openSpace } from "@/api/spaces";
import { ApiError, errorText } from "@/lib/api";
import type { Space } from "@/types/daemon";

export interface InflightCounts {
  dispatch: number;
  foreign: number;
}

export interface CloseSpaceState {
  close: () => void;
  forceClose: () => void;
  dismiss: () => void;
  /** 非空即表示 409 已发生，Dialog 要开。 */
  pending: InflightCounts | null;
  closing: boolean;
  forcing: boolean;
  error: unknown;
}

export function useCloseSpace(wsId: string): CloseSpaceState {
  const queryClient = useQueryClient();
  const [pending, setPending] = useState<InflightCounts | null>(null);

  const mutation = useMutation({
    mutationFn: (force: boolean) => closeSpace(wsId, force),
    onSuccess: () => {
      setPending(null);
      toast.success("空间已关闭");
      void queryClient.invalidateQueries({ queryKey: ["space", wsId] });
      void queryClient.invalidateQueries({ queryKey: ["spaces"] });
      void queryClient.invalidateQueries({ queryKey: ["space-tasks", wsId] });
    },
    onError: (error) => {
      // 409 不是失败而是「需要用户再决定一次」：Dialog 不关，改展示计数。
      if (error instanceof ApiError && error.code === "inflight_tasks") {
        setPending({
          dispatch: error.inflightDispatch ?? 0,
          foreign: error.inflightForeign ?? 0,
        });
        return;
      }
      toast.error(errorText(error));
    },
  });

  return {
    close: () => mutation.mutate(false),
    forceClose: () => mutation.mutate(true),
    dismiss: () => {
      setPending(null);
      mutation.reset();
    },
    pending,
    closing: mutation.isPending && pending === null,
    forcing: mutation.isPending && pending !== null,
    error: mutation.error,
  };
}

export interface OpenSpaceState {
  open: () => void;
  opening: boolean;
}

/** 打开相位。`open` 是终态：到了它这一层就没有话要说了。 */
export type SpaceOpenPhase =
  "open" | "opening" | "failed" | "closed" | "dormant";

export type SpaceOpenNoticePhase = Exclude<SpaceOpenPhase, "open">;

export interface OpenSpaceOnEnterState {
  phase: SpaceOpenPhase;
  /** 失败原因；由横幅交给用户，这一层不弹 toast。 */
  error: unknown;
  /** 显式重开：失败后的「重试」与关闭态的「重新打开」共用它。 */
  retry: () => void;
}

/**
 * 进入一个未打开的空间时自动打开它，每个 `ws_id` 只自动尝试一次。
 *
 * 自动打开只覆盖「未打开」（`closed_at` 为空）：`closed_at` 有值代表用户显式
 * 关过它，重开必须是他的动作（`retry`）。否则一次路由往返、或另一个标签页的
 * 15s 轮询，都会把「关闭」撤销掉 —— 关闭是数据上的事实，判据也只能落在数据上。
 *
 * 「一次」由 ref 兜住：空间详情每 15s 轮询一次，成功失败都换回来一份新对象，
 * 没有这道闸，一次失败会被轮询放大成每 15s 一次的自动重试 —— 用户没点，
 * 界面却在反复报错。重发是用户的动作（`retry`），不是轮询的副作用。
 *
 * 重开店在重挂载时再发一次也无妨：`POST /open` 不重建实例（已开则原样回视图），
 * 只是成功打开会刷新 `last_opened_at`。
 */
export function useOpenSpaceOnEnter(
  space: Space | undefined,
): OpenSpaceOnEnterState {
  const queryClient = useQueryClient();
  const attempted = useRef<string | null>(null);

  const mutation = useMutation({
    mutationFn: (wsId: string) => openSpace(wsId),
    onSuccess: (opened, wsId) => {
      // 自动动作不弹 toast：用户只是进了这个空间，没有「完成了一件事」。
      // 先把服务端回的那一份落进缓存：等重取回来再改的话，旧对象会让
      // 「还没打开」多闪一帧，而这一帧正好落在用户刚点完成功的那一刻。
      queryClient.setQueryData(["space", wsId], opened);
      void queryClient.invalidateQueries({ queryKey: ["space", wsId] });
      void queryClient.invalidateQueries({ queryKey: ["spaces"] });
      void queryClient.invalidateQueries({ queryKey: ["space-tasks", wsId] });
    },
  });
  const { mutate } = mutation;

  useEffect(() => {
    if (!space) return;
    if (space.status === "open" || space.closed_at !== null) return;
    if (attempted.current === space.ws_id) return;
    attempted.current = space.ws_id;
    mutate(space.ws_id);
  }, [space, mutate]);

  const current = space?.ws_id;
  const opening = mutation.isPending && mutation.variables === current;
  // 页面在 A→B 之间不重挂载：上一轮的状态不能算到下一个空间头上。
  const failed = mutation.variables === current ? mutation.error : null;

  return {
    phase: phaseOf(space, { opening, failed }),
    error: failed,
    retry: () => {
      // 显式动作，不经过 `attempted` 那道闸：用户点一次就发一次。
      if (current) mutate(current);
    },
  };
}

/**
 * 相位只看两处：详情里的数据（`status` / `closed_at`）与本轮请求的状态。
 * `closed` 与 `dormant` 的差别全在 `closed_at` 上 —— 一个是用户关的，
 * 一个是还没开过，文案与后续动作都不同，不能让一句话同时承担。
 */
function phaseOf(
  space: Space | undefined,
  run: { opening: boolean; failed: unknown },
): SpaceOpenPhase {
  if (run.opening) return "opening";
  if (!space) return "dormant";
  if (space.status === "open") return "open";
  if (run.failed) return "failed";
  return space.closed_at !== null ? "closed" : "dormant";
}

/**
 * 手动「打开」按钮：`POST /api/spaces` 对同一 `work_dir` 是幂等的（有行就开、
 * 没有就建），所以目录与团队原样提交就够。
 *
 * 进入空间时的自动打开走 `useOpenSpaceOnEnter`：那条路径手上没有表单数据，
 * 拿列表里可能过期的 `work_dir` 去开会错位，它只回传 `ws_id`。
 */
export function useOpenSpace(
  wsId: string,
  workDir: string,
  teamId: string,
): OpenSpaceState {
  const queryClient = useQueryClient();
  const mutation = useMutation({
    mutationFn: () => createSpace({ work_dir: workDir, team_id: teamId }),
    onSuccess: () => {
      toast.success("空间已打开");
      void queryClient.invalidateQueries({ queryKey: ["space", wsId] });
      void queryClient.invalidateQueries({ queryKey: ["spaces"] });
    },
    onError: (error) => toast.error(errorText(error)),
  });

  return { open: () => mutation.mutate(), opening: mutation.isPending };
}
