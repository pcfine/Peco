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

  // 团队详情是 mid 档：容器锁在这里，`TeamConfigPanel` 自己不锁宽 ——
  // `/peco` 复用同一份面板时它的配置面是 wide。
  return (
    <div className="mx-auto h-full w-full max-w-mid">
      <TeamConfigPanel teamId={teamId} wsId={wsId} showBackToList />
    </div>
  );
}
