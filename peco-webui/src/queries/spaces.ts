import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import {
  TRASH_CAPACITY,
  listSpaces,
  listTrashSpaces,
  purgeSpace,
  restoreSpace,
  trashSpace,
} from "@/api/spaces";
import { errorText } from "@/lib/api";
import type { TrashSpace } from "@/types/daemon";

export const spacesKey = ["spaces"] as const;

/**
 * 回收站挂在 `spacesKey` 之下：空间进了回收站，清单与回收站是同一件事的两面，
 * 一次 `spacesKey` 失效就把两处一起更新，不会漏。
 */
export const trashSpacesKey = ["spaces", "trash"] as const;

/** 空间清单 15s 一次：打开/关闭是别的入口触发的，本地没有推送源。 */
export function useSpaces() {
  return useQuery({
    queryKey: spacesKey,
    queryFn: listSpaces,
    refetchInterval: 15_000,
  });
}

/** 回收站清单 15s 一次，节奏与空间清单一致：两个标签页看到的应是同一份回收站。 */
export function useTrashSpaces() {
  return useQuery({
    queryKey: trashSpacesKey,
    queryFn: listTrashSpaces,
    refetchInterval: 15_000,
  });
}

export interface RemovalRequest {
  wsId: string;
  /** 越过在飞任务门槛；只有 409 之后用户再点一次才会为真。 */
  force: boolean;
}

/**
 * 移入回收站：会话与记忆不动，撤销的路径是回收站的「恢复」。
 *
 * 回收站满了会同步销毁最旧一条，daemon 不在回执里说是哪一条 —— 只能靠本地这份
 * 清单的长度判断「这次移入挤掉了别人」，并在同一条提示里说清楚。
 * 失败不在这里弹 toast：发起它的确认弹窗还开着，它自己会把原文案摆在用户眼前。
 */
export function useTrashSpace() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ wsId, force }: RemovalRequest) => trashSpace(wsId, force),
    onSuccess: () => {
      const before =
        queryClient.getQueryData<TrashSpace[]>(trashSpacesKey)?.length ?? 0;
      toast.success(
        before >= TRASH_CAPACITY
          ? `已移入回收站，回收站已满 ${TRASH_CAPACITY} 条，最早的一项已被自动销毁`
          : "已移入回收站",
      );
      void queryClient.invalidateQueries({ queryKey: spacesKey });
    },
  });
}

/** 永久销毁：会话与记忆都没了，所以详情缓存直接丢掉，不留一份指向死空间的副本。 */
export function usePurgeSpace() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ wsId, force }: RemovalRequest) => purgeSpace(wsId, force),
    onSuccess: (_result, { wsId }) => {
      queryClient.removeQueries({ queryKey: ["space", wsId] });
      toast.success("空间已永久销毁");
      void queryClient.invalidateQueries({ queryKey: spacesKey });
    },
  });
}

/**
 * 恢复：空间回到清单的「已关闭」组，用户自己决定要不要打开。
 *
 * 没有确认弹窗，所以失败必须弹出来 —— 否则点完「恢复」什么都没发生。
 */
export function useRestoreSpace() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (wsId: string) => restoreSpace(wsId),
    onSuccess: (_result, wsId) => {
      toast.success("空间已恢复");
      void queryClient.invalidateQueries({ queryKey: spacesKey });
      void queryClient.invalidateQueries({ queryKey: ["space", wsId] });
    },
    onError: (error) => toast.error(errorText(error)),
  });
}
