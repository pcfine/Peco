// Peco 右栏的「总览」：四块既有面板纵向堆叠。
//
// 不复用看板的拖拽栅格 —— 排序与显隐属于旧看板，本期不进新壳。每块外面留一层
// 带 `panel-*` 标记的容器：面板的身份与 e2e / 冒烟测试挂钩的那套命名不变。
//
// 这里再声明一个同名 `work` 容器：四块面板的宽档断点（`@min-[640px]/work`）
// 是按整页宽度写的，落到 320px 的右栏里会硬套宽档排版把列挤爆；同名容器取最近的
// 祖先，于是同一批面板按栏宽排版，不需要给面板加第二套变体。

import { PanelResults } from "@/components/board/panel-results";
import { PanelSpaces } from "@/components/board/panel-spaces";
import { PanelTeams } from "@/components/board/panel-teams";
import { PanelToday } from "@/components/board/panel-today";
import { usePeco } from "@/queries/peco";

export function SpaceOverviewTab() {
  const { data: peco } = usePeco();

  return (
    <div className="@container/work h-full divide-y divide-line overflow-y-auto">
      <div data-testid="panel-today">
        <PanelToday pecoWsId={peco?.ws_id ?? null} />
      </div>
      <div data-testid="panel-spaces">
        <PanelSpaces />
      </div>
      <div data-testid="panel-teams">
        <PanelTeams />
      </div>
      <div data-testid="panel-results">
        <PanelResults />
      </div>
    </div>
  );
}
