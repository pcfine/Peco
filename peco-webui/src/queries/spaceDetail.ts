// 空间详情的数据源：路由自带 `wsId`，queryKey 必须含它 ——
// 否则从 A 空间跳到 B 空间会先命中 A 的缓存，表格闪一次别人的数据。

import { useQuery } from "@tanstack/react-query";
import {
  getSpace,
  spaceArtifacts,
  spaceFiles,
  spaceLiveTasks,
  spaceTasks,
} from "@/api/spaces";
import { liveInflightCount } from "@/lib/liveTasks";
import { TASK_RUNNING_STATES } from "@/lib/taskStates";

export function useSpace(wsId: string) {
  return useQuery({
    queryKey: ["space", wsId],
    queryFn: () => getSpace(wsId),
    // 打开/关闭由本页动作触发，15s 只是兜底（别的浏览器标签也可能改它）。
    refetchInterval: 15_000,
  });
}

export function useSpaceTasks(wsId: string) {
  return useQuery({
    queryKey: ["space-tasks", wsId],
    queryFn: () => spaceTasks(wsId),
    refetchInterval: 15_000,
    placeholderData: (prev) => prev,
  });
}

/**
 * 实时列表的轮询节奏：3 秒是「看得见在动」与请求量之间的折中。
 *
 * 单任务的 `/tasks/{id}/stream` 成本更高（每行一条长连接），留到确认轮询不够快时
 * 再对展开行单独接。
 */
export const LIVE_TASKS_POLL_MS = 3_000;

/**
 * 该空间 TaskHub 的实时快照。空间没打开是 409，由调用方回落到台账（这一层不吞错）。
 *
 * `enabled: false` 时一个请求都不发：隐藏的空间不该因为角标而轮询。
 */
export function useSpaceLiveTasks(
  wsId: string,
  options: { enabled?: boolean } = {},
) {
  return useQuery({
    queryKey: ["space-live-tasks", wsId],
    queryFn: () => spaceLiveTasks(wsId),
    refetchInterval: LIVE_TASKS_POLL_MS,
    enabled: options.enabled ?? true,
    // 切空间后旧值先留着，面板不会先闪一帧骨架再回到同样的列表。
    placeholderData: (prev) => prev,
  });
}

/**
 * 在飞任务数：实时快照优先（含等在补输入/授权的任务），拿不到（还在加载、409
 * 或出错）就退回台账 —— 提示宁可少说一个，也不该因为一个接口抖动就整块消失。
 */
export function useInflightTaskCount(
  wsId: string,
  options: { enabled?: boolean } = {},
): number {
  const live = useSpaceLiveTasks(wsId, options);
  const ledger = useSpaceTasks(wsId);
  const fromLive = liveInflightCount(live.data);
  if (fromLive !== null) return fromLive;
  return (ledger.data ?? []).filter((task) =>
    TASK_RUNNING_STATES.includes(task.task_state),
  ).length;
}

export function useSpaceArtifacts(wsId: string) {
  return useQuery({
    queryKey: ["space-artifacts", wsId],
    queryFn: () => spaceArtifacts(wsId),
    refetchInterval: 15_000,
    placeholderData: (prev) => prev,
  });
}

/**
 * 目录 / 文件内容。`path` 为空串即根目录；未打开的空间会 409，
 * 由调用方展示原文案（这里不把错误翻译成空列表）。
 */
export function useSpaceFiles(
  wsId: string,
  path: string,
  options: { enabled?: boolean } = {},
) {
  return useQuery({
    queryKey: ["space-files", wsId, path],
    queryFn: () => spaceFiles(wsId, { path }),
    enabled: options.enabled ?? true,
    placeholderData: (prev) => prev,
  });
}
