import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import {
  createSchedule,
  deleteSchedule,
  listScheduleRuns,
  listSchedules,
  runSchedule,
  updateSchedule,
} from "@/api/schedules";
import type {
  CreateScheduleRequest,
  UpdateScheduleRequest,
} from "@/types/daemon";

export const schedulesKey = ["schedules"] as const;

/** 运行记录挂在列表 key 之下：任务被改/删时，展开着的历史一起重取。 */
export function scheduleRunsKey(id: string) {
  return ["schedules", id, "runs"] as const;
}

/** 列表 10s：下次运行时刻与最近状态靠它推进，本地没有推送源。 */
export function useSchedules() {
  return useQuery({
    queryKey: schedulesKey,
    queryFn: listSchedules,
    refetchInterval: 10_000,
  });
}

/** 展开着的历史 5s：一轮跑完到状态翻面之间只差这一次轮询。 */
export function useScheduleRuns(id: string) {
  return useQuery({
    queryKey: scheduleRunsKey(id),
    queryFn: () => listScheduleRuns(id, 20),
    refetchInterval: 5_000,
  });
}

export function useCreateSchedule() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: CreateScheduleRequest) => createSchedule(body),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: schedulesKey });
    },
  });
}

/**
 * 改名/改表达式/开关启用都走这一条。
 *
 * 成功不弹 toast：开关与行内文字立刻更新，再补一句话只是噪音。用 `enabled` 这类
 * 单个字段时省略其余字段即「不改」，不需要先回读整行再拼一份完整请求体。
 */
export function useUpdateSchedule() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id: string; body: UpdateScheduleRequest }) =>
      updateSchedule(id, body),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: schedulesKey });
    },
  });
}

export function useDeleteSchedule() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => deleteSchedule(id),
    onSuccess: (_result, id) => {
      queryClient.removeQueries({ queryKey: scheduleRunsKey(id) });
      void queryClient.invalidateQueries({ queryKey: schedulesKey });
      toast.success("定时任务已删除");
    },
  });
}

export function useRunSchedule() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => runSchedule(id),
    onSuccess: (accepted, id) => {
      if (accepted.status === "failed") {
        // 回执里没有 schedule_id：历史与列表都按入参 id 失效。
        toast.error("启动失败，打开运行历史看原因");
        void queryClient.invalidateQueries({ queryKey: scheduleRunsKey(id) });
      } else {
        toast.success(
          accepted.status === "pending"
            ? "目标会话正忙，已排队等待重试"
            : "已开始运行",
        );
      }
      void queryClient.invalidateQueries({ queryKey: schedulesKey });
    },
  });
}
