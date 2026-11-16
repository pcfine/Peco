// `/teams/:teamId` —— 团队页，与 Peco 聊天页左头的 `·peco` 共用 `TeamConfigPanel`。
//
// 这里补团队页自己的两件事：从空间列表里挑一个已打开空间借道（没有也能用 ——
// 资源 tab 会按团队寻址），以及出错时回团队列表的路。
//
// Peco 是后端保留团队，特例化两层：能力根固定挂在 Peco 空间上、记忆直接浏览
// Peco 空间自己的记忆；「绑定空间」整栏用 hideSpaces 收掉 —— 连带它内容区的
// 「新建空间」入口。

import { useParams } from "react-router-dom";
import { TeamConfigPanel } from "@/components/teams/TeamConfigPanel";
import { usePeco } from "@/queries/peco";
import { useSpaces } from "@/queries/spaces";

const PECO_TEAM_ID = "peco";

export function TeamDetailPage() {
  const { teamId = "" } = useParams();
  const peco = usePeco();
  const spaces = useSpaces();
  const isPeco = teamId === PECO_TEAM_ID;

  const pecoSpace =
    (spaces.data ?? []).find((space) => space.ws_id === peco.data?.ws_id) ??
    null;

  // 团队是能力边界：哪个已打开空间作请求头不影响结果。Peco 只听身份里的 ws_id。
  const wsId = isPeco
    ? pecoSpace?.status === "open"
      ? pecoSpace.ws_id
      : null
    : ((spaces.data ?? []).find(
        (space) => space.team_id === teamId && space.status === "open",
      )?.ws_id ?? null);

  return (
    // 整页就是「左分类导航 + 右内容」两侧，容器不居中封宽：
    // 封到 1200 后宽屏会在 rail 与分类导航之间留出几百像素空白。
    <div className="h-full w-full">
      <TeamConfigPanel
        teamId={teamId}
        wsId={wsId}
        showBackToList
        hideSpaces={isPeco}
        // 空间没打开时也传：记忆面板自己会摆「打开空间」的入口，
        // 比团队维度的「尚未接入」占位更靠近用户想做的那件事。
        memoryWsId={isPeco ? (peco.data?.ws_id ?? null) : null}
      />
    </div>
  );
}
