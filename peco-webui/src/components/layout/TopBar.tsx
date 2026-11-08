// 顶栏：位置指示 + 页面动作槽 + dock 开关。视口 <900 时 rail 离屏，
// 空间切换器搬到这里（同一枚组件，不另造状态）；<1100 时 rail 只剩图标轨，
// 汉堡按钮改开整栏 Sheet。

import { useLocation } from "react-router-dom";
import { Menu, MessagesSquare } from "lucide-react";
import { SpaceSwitcher } from "@/components/layout/SpaceSwitcher";
import { useMediaQuery } from "@/hooks/useMediaQuery";
import { usePecoTodayTasks } from "@/queries/tasks";
import { TASK_RUNNING_STATES } from "@/lib/taskStates";
import { usePeco } from "@/queries/peco";
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
function titleOf(pathname: string, search: string): string {
  // Peco 的两个面共用一个路径：标题跟着 `face` 走，否则翻转后顶栏还在说聊天。
  if (pathname === "/peco") {
    return new URLSearchParams(search).get("face") === "config"
      ? "Peco 能力配置"
      : "Peco 聊天";
  }
  if (PAGE_TITLES[pathname]) return PAGE_TITLES[pathname];
  if (/^\/spaces\/[^/]+\/status$/.test(pathname)) return "空间状态";
  if (pathname.startsWith("/spaces/")) return "空间聊天";
  if (pathname.startsWith("/teams/")) return "团队配置";
  return "";
}

export function TopBar({ onOpenNav }: { onOpenNav: () => void }) {
  const { pathname, search } = useLocation();
  // 900–1099 的左栏只剩图标轨，导航项的字全藏了，完整导航得从 Sheet 取。
  const railOffscreen = !useMediaQuery("(min-width: 900px)");
  const navInSheet = !useMediaQuery("(min-width: 1100px)");
  const { data: peco } = usePeco();
  const { data: tasks } = usePecoTodayTasks(peco?.ws_id ?? null);
  const dockOpen = useDockStore((state) => state.open);
  const toggleDock = useDockStore((state) => state.toggle);

  // 角标 = 在飞数（Peco 发起的 submitted/working）；0 时不渲染，不显示「0」。
  const inflight = (tasks ?? []).filter((task) =>
    TASK_RUNNING_STATES.includes(task.task_state),
  ).length;

  return (
    <header className="flex h-11 shrink-0 items-center gap-2 border-b border-line bg-canvas px-3">
      {navInSheet && (
        <button
          type="button"
          onClick={onOpenNav}
          aria-label="打开导航"
          className="grid h-7 w-7 place-items-center rounded-[4px] text-ink-muted hover:bg-line hover:text-ink"
        >
          <Menu className="h-4 w-4" />
        </button>
      )}
      {railOffscreen && <SpaceSwitcher />}
      <h1 className="truncate text-sm font-semibold text-ink">
        {titleOf(pathname, search)}
      </h1>

      <div className="ml-auto flex items-center gap-1">
        <button
          type="button"
          onClick={toggleDock}
          aria-expanded={dockOpen}
          aria-controls="peco-dock"
          aria-label="Peco 对话"
          className="relative grid h-7 w-7 place-items-center rounded-[4px] text-ink-muted hover:bg-line hover:text-ink"
        >
          <MessagesSquare className="h-4 w-4" />
          {inflight > 0 && (
            <span className="absolute -right-0.5 -top-0.5 grid h-3.5 min-w-3.5 place-items-center rounded-full bg-primary-fill px-0.5 tabular-nums text-[9px] leading-none text-white">
              {inflight}
            </span>
          )}
        </button>
      </div>
    </header>
  );
}
