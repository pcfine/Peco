import { useQuery } from "@tanstack/react-query";
import { listTasks, type TaskFilter } from "@/api/tasks";

interface TaskQueryOptions {
  /** 轮询间隔；看板今日列 5s、成果流 15s，统一成一个值会让某块要么太慢要么白打请求。 */
  refetchInterval?: number;
  enabled?: boolean;
}

export function useTasks(filter: TaskFilter, options: TaskQueryOptions = {}) {
  return useQuery({
    queryKey: ["tasks", filter],
    queryFn: () => listTasks(filter),
    refetchInterval: options.refetchInterval,
    enabled: options.enabled ?? true,
    // 切空间后旧值先别丢：表格直接清空会让滚动位置与骨架一起跳。
    placeholderData: (prev) => prev,
  });
}

/** Peco 派发的今日任务：看板今日列与 dock 角标共用同一份。 */
export function usePecoTodayTasks(pecoWsId: string | null) {
  return useTasks(
    { origin_ws_id: pecoWsId ?? undefined, limit: 200 },
    // 身份还没到手时别发请求：没有 origin 过滤就是把整张台账拉回来。
    {
      refetchInterval: pecoWsId ? 5_000 : undefined,
      enabled: pecoWsId !== null,
    },
  );
}

/** 最近完成的成果：成果流面板与 rail 的「今日」计数共用。 */
export function useRecentResults(limit = 50) {
  return useTasks(
    { task_state: "completed", limit },
    { refetchInterval: 15_000 },
  );
}
