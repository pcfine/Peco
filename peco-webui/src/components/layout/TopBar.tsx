// 顶栏：只有在窄屏才存在（≥1100 由 AppShell 直接不渲染它）—— 位置指示、导航入口，
// <900 时 rail 离屏，空间切换器与抽屉开关搬到这里（同一枚组件，不另造状态）。

import { useLocation } from "react-router-dom";
import { Menu, MessagesSquare } from "lucide-react";
import { SpaceSwitcher } from "@/components/layout/SpaceSwitcher";
import { useMediaQuery } from "@/hooks/useMediaQuery";
import { usePecoTodayTasks } from "@/queries/tasks";
import { TASK_RUNNING_STATES } from "@/lib/taskStates";
import { usePeco } from "@/queries/peco";
import { isPecoSurface } from "@/lib/pecoSurface";
import { useDockStore } from "@/stores/dockStore";

const PAGE_TITLES: Record<string, string> = {
  "/spaces": "工作室",
  "/spaces/new": "新建空间",
  "/teams": "团队",
  "/results": "成果",
  "/schedules": "定时任务",
  "/settings": "设置",
};

/** 动态段（`/spaces/:wsId`、`/teams/:teamId`）只按前缀取标题。 */
function titleOf(pathname: string): string {
  if (pathname === "/peco") return "Peco 聊天";
  if (PAGE_TITLES[pathname]) return PAGE_TITLES[pathname];
  if (pathname.startsWith("/spaces/")) return "空间聊天";
  if (pathname.startsWith("/teams/")) return "团队配置";
  return "";
}

export function TopBar({ onOpenNav }: { onOpenNav: () => void }) {
  const { pathname } = useLocation();
  // <900 时 rail 离屏，抽屉开关只能落在这里；900–1099 走左栏那枚图标。
  const railOffscreen = !useMediaQuery("(min-width: 900px)");
  const { data: peco } = usePeco();
  // 抽屉不在 Peco 会话本体所在的页面上存在，开关也就没有落点。
  const onPecoSurface = isPecoSurface(pathname, peco?.ws_id);
  const { data: tasks } = usePecoTodayTasks(peco?.ws_id ?? null);
  const dockOpen = useDockStore((state) => state.open);
  const toggleDock = useDockStore((state) => state.toggle);

  // 角标 = 在飞数（Peco 发起的 submitted/working）；0 时不渲染，不显示「0」。
  const inflight = (tasks ?? []).filter((task) =>
    TASK_RUNNING_STATES.includes(task.task_state),
  ).length;

  return (
    <header
      data-slot="topbar"
      className="flex h-11 shrink-0 items-center gap-2 border-b border-line bg-canvas px-3"
    >
      <button
        type="button"
        onClick={onOpenNav}
        aria-label="打开导航"
        className="grid h-7 w-7 place-items-center rounded-sm text-ink-muted hover:bg-line hover:text-ink"
      >
        <Menu className="h-4 w-4" />
      </button>
      {railOffscreen && <SpaceSwitcher />}
      <p className="truncate text-caption font-medium text-ink-muted">
        {titleOf(pathname)}
      </p>

      {railOffscreen && !onPecoSurface && (
        <div className="ml-auto flex items-center gap-1">
          <button
            type="button"
            onClick={toggleDock}
            aria-expanded={dockOpen}
            aria-controls="peco-dock"
            aria-label="Peco 对话"
            className="relative grid h-7 w-7 place-items-center rounded-sm text-ink-muted hover:bg-line hover:text-ink"
          >
            <MessagesSquare className="h-4 w-4" />
            {inflight > 0 && (
              <span className="absolute -right-0.5 -top-0.5 grid h-4 min-w-4 place-items-center rounded-full bg-primary-fill px-0.5 tabular-nums text-2xs leading-none text-white">
                {inflight}
              </span>
            )}
          </button>
        </div>
      )}
    </header>
  );
}
