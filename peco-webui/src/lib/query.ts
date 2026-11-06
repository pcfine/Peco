import { QueryClient } from "@tanstack/react-query";

/**
 * `staleTime` 取 5s：看板与列表靠轮询推进，缓存只负责「切走再切回不闪骨架」，
 * 超过 5s 的旧值在轮询周期内本就会被替换，留更久只会让切空间后的旧数据看起来
 * 像新数据。需要更快的地方（今日任务 5s、在飞 5s）由各自的 `refetchInterval`
 * 覆盖，不在这里统一放宽。
 */
export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 5_000,
      // 失败一次就报错：daemon 在本机，重试只会在「它没起来」时多等两轮。
      retry: 0,
      refetchOnWindowFocus: true,
    },
  },
});
