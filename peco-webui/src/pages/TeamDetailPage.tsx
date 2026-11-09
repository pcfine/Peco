// `/teams/:teamId` —— 只是一个薄包装：团队页与 Peco 翻转面共用 `TeamConfigPanel`，
// 这里只补团队页自己的两件事：从空间列表里挑一个已打开空间作 `X-Peco-Space`，
// 以及出错时回团队列表的路。

import { useParams } from "react-router-dom";
import { TeamConfigPanel } from "@/components/teams/TeamConfigPanel";
import { useSpaces } from "@/queries/spaces";

export function TeamDetailPage() {
  const { teamId = "" } = useParams();
  const spaces = useSpaces();

  // 团队是能力边界：哪个已打开空间作请求头不影响结果，取第一个。
  const wsId =
    (spaces.data ?? []).find(
      (space) => space.team_id === teamId && space.status === "open",
    )?.ws_id ?? null;

  // 整页就是「左分类导航 + 右内容」两侧，容器不再居中封宽：
  // 封到 1200 后宽屏会在 rail 与分类导航之间留出几百像素空白。
  // `/peco` 翻转面复用同一份面板，宽度由那一面自己决定。
  return (
    <div className="h-full w-full">
      <TeamConfigPanel teamId={teamId} wsId={wsId} showBackToList />
    </div>
  );
}
