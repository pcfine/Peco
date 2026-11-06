// `/` Peco 面板：四块可组合面板。整块看板的顺序与显隐来自 boardStore（localStorage）。

import { useEffect, useRef } from "react";
import { BoardGrid } from "@/components/board/BoardGrid";
import { PanelResults } from "@/components/board/panel-results";
import { PanelSpaces } from "@/components/board/panel-spaces";
import { PanelTeams } from "@/components/board/panel-teams";
import { PanelToday } from "@/components/board/panel-today";
import { usePeco } from "@/queries/peco";
import { useSpaceStore } from "@/stores/spaceStore";

export function PecoBoardPage() {
  const { data: peco } = usePeco();
  const currentWsId = useSpaceStore((state) => state.currentWsId);
  const scrollerRef = useRef<HTMLDivElement>(null);

  // 切空间 = 整板换人：滚动位置回到顶部（工作室列表则保持不动）。
  useEffect(() => {
    const main = scrollerRef.current?.closest("main");
    if (main) main.scrollTop = 0;
  }, [currentWsId]);

  return (
    <div ref={scrollerRef}>
      <BoardGrid
        panels={{
          today: <PanelToday pecoWsId={peco?.ws_id ?? null} />,
          spaces: <PanelSpaces />,
          teams: <PanelTeams />,
          results: <PanelResults />,
        }}
      />
    </div>
  );
}
