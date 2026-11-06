// 空间的开关动作。关闭被 409 拦下时，两个在飞计数直接来自那一刻的响应体，
// 不从空间详情里另取一份 —— 详情里的是 15s 前的数，而挡住决定的就是这一刻的数。

import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { closeSpace, createSpace } from "@/api/spaces";
import { ApiError, errorText } from "@/lib/api";

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

/**
 * 重新打开一个已关闭的空间。
 *
 * `POST /api/spaces` 对同一 `work_dir` 是幂等的（有行就开、没有就建），所以
 * 「打开」不需要第二条端点：目录与团队都在库里，原样提交即可。
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
