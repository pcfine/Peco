// 切换当前空间的唯一实现：rail 总线行、窄屏切换器、详情页的「设为当前」都调它。
//
// 两件事必须一起发生，分成两处实现迟早会漏掉一件：
// 1. 写 store（后续请求头的归属）；
// 2. 详情页跟过去（URL 里的 wsId 才是页面数据的主人，不跟过去就是两套事实打架）。
//
// 滚动位置不在这里处理：`/` 重置、`/spaces` 保持的规则由各自页面持有。

import { useCallback } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { useSpaceStore } from "@/stores/spaceStore";

export function useSwitchSpace(): (wsId: string) => void {
  const navigate = useNavigate();
  const { pathname, search } = useLocation();
  const queryClient = useQueryClient();
  const setCurrent = useSpaceStore((state) => state.setCurrent);

  return useCallback(
    (wsId: string) => {
      setCurrent(wsId);

      // `/spaces/new` 也是 `/spaces/` 前缀，但它不是详情页。
      if (/^\/spaces\/[^/]+$/.test(pathname) && pathname !== "/spaces/new") {
        navigate(`/spaces/${encodeURIComponent(wsId)}${search}`, {
          replace: true,
        });
      }

      void queryClient.invalidateQueries({ queryKey: ["tasks"] });
      void queryClient.invalidateQueries({ queryKey: ["conversations"] });
    },
    [navigate, pathname, search, queryClient, setCurrent],
  );
}
