// 空间详情的数据源：路由自带 `wsId`，queryKey 必须含它 ——
// 否则从 A 空间跳到 B 空间会先命中 A 的缓存，表格闪一次别人的数据。

import { useQuery } from "@tanstack/react-query";
import { getSpace, spaceArtifacts, spaceFiles, spaceTasks } from "@/api/spaces";

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
