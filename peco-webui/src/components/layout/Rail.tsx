// 左栏：品牌 / Peco / 打开集 / 工作室·团队·成果 / 固定底栏。
// 打开集是唯一的空间列表 —— 未打开的空间只在工作室页里，左栏不是索引；
// 「工作室」入口同时承担打开空间。

import { type KeyboardEvent } from "react";
import { Link, useLocation, useNavigate } from "react-router-dom";
import {
  Clock,
  LayoutGrid,
  Moon,
  Package,
  PanelLeftClose,
  PanelLeftOpen,
  Settings,
  Sun,
  UserCog,
  Users,
} from "lucide-react";
import { BrandMark } from "@/components/common/BrandMark";
import { OpenSpaceRow } from "@/components/layout/OpenSpaceRow";
import { useActivateSpace } from "@/hooks/useOpenSpaces";
import { usePeco } from "@/queries/peco";
import { useSpaces } from "@/queries/spaces";
import { useRecentResults } from "@/queries/tasks";
import { useTeams } from "@/queries/teams";
import { useDockStore } from "@/stores/dockStore";
import { useOpenSpacesStore } from "@/stores/openSpacesStore";
import { useThemeStore } from "@/stores/themeStore";
import { useUiStore } from "@/stores/uiStore";
import { isToday } from "@/lib/relative";
import { cn } from "@/lib/utils";
import type { Space } from "@/types/daemon";

/**
 * 当前路由落在哪个空间上：详情页与状态页都算这个空间。
 * `/spaces`、`/spaces/new` 是工作室栏目，不归到任何空间行。
 */
function spaceIdInPath(pathname: string): string | null {
  const match = /^\/spaces\/([^/]+)(?:\/status)?$/.exec(pathname);
  if (!match || match[1] === "new") return null;
  try {
    return decodeURIComponent(match[1]);
  } catch {
    return match[1];
  }
}

export function Rail({
  collapsed = false,
  collapsible = true,
}: {
  collapsed?: boolean;
  /** 视口容不容得下 208 ↔ 64 的切换：容不下时宽度是定值，就不要再给一个按钮。 */
  collapsible?: boolean;
}) {
  const railOpen = useUiStore((state) => state.railOpen);
  const setRailOpen = useUiStore((state) => state.setRailOpen);
  const dockOpen = useDockStore((state) => state.open);
  const toggleDock = useDockStore((state) => state.toggle);
  const theme = useThemeStore((state) => state.theme);
  const setTheme = useThemeStore((state) => state.setTheme);
  const openIds = useOpenSpacesStore((state) => state.openIds);
  const activeWsId = useOpenSpacesStore((state) => state.activeWsId);
  const removeOpen = useOpenSpacesStore((state) => state.close);
  const setActive = useOpenSpacesStore((state) => state.setActive);
  const activateSpace = useActivateSpace();
  const navigate = useNavigate();
  const { pathname } = useLocation();

  const { data: peco } = usePeco();
  const { data: spaces } = useSpaces();
  const { data: teams } = useTeams();
  const { data: results } = useRecentResults();

  // 打开集只存 id：名称、团队、在飞数都从空间列表这一份事实里取。
  const spaceById = new Map(
    (spaces ?? []).map((space) => [space.ws_id, space]),
  );
  const rows = openIds.reduce<Space[]>((acc, id) => {
    // Peco 有自己的行，重复出现两次只会让人怀疑哪个才是总管。
    if (id === peco?.ws_id) return acc;
    const space = spaceById.get(id);
    if (space) acc.push(space);
    return acc;
  }, []);
  const todayResults = (results ?? []).filter((task) =>
    isToday(task.terminal_at ?? task.created_at),
  ).length;

  // 与空间行一致：只切激活态。Peco 的能力配置从聊天页左头的 `·peco` 链接进团队配置页。
  const openPeco = () => {
    if (!peco) return;
    setActive(peco.ws_id);
    if (pathname !== "/peco") navigate("/peco");
  };

  // `×` 只移出打开集，不关闭空间。移掉的若正是当前空间，激活态不能就此悬空 ——
  // 打开集是唯一的空间列表，没有一行能指「当前」就是自相矛盾的界面。移掉别的行
  // 不动路由，也不切激活态。
  const removeSpace = (wsId: string) => {
    const removedActive = activeWsId === wsId;
    removeOpen(wsId);
    if (!removedActive) return;
    const nextWsId = openIds.find((id) => id !== wsId && id !== peco?.ws_id);
    if (nextWsId) {
      activateSpace(nextWsId);
      return;
    }
    navigate("/spaces");
  };

  // 行之间 ↑/↓ 移动焦点：行本身是按钮，Enter/Space 天然生效。
  const moveFocus = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    const rowNodes = Array.from(
      event.currentTarget.querySelectorAll<HTMLElement>("[data-space-row]"),
    );
    const index = rowNodes.indexOf(document.activeElement as HTMLElement);
    if (index === -1) return;
    event.preventDefault();
    const next = event.key === "ArrowDown" ? index + 1 : index - 1;
    rowNodes[Math.min(Math.max(next, 0), rowNodes.length - 1)]?.focus();
  };

  // 边框只跟当前路由：在哪个栏目/空间，哪一行才高亮。
  const routeWsId =
    pathname === "/peco" ? (peco?.ws_id ?? null) : spaceIdInPath(pathname);
  const onSpacesPage = pathname === "/spaces" || pathname === "/spaces/new";
  const onTeamsPage = pathname === "/teams" || pathname.startsWith("/teams/");
  const onResultsPage = pathname === "/results";
  const onSchedulesPage = pathname === "/schedules";
  const onSettingsPage = pathname === "/settings";

  // 栏目入口的两种排版共用一串底类：收起态只留图标并居中；激活态与空间行同一枚胶囊。
  const navRowClass = (active: boolean) =>
    cn(
      "mx-2 flex h-8 items-center rounded-lg border text-body font-normal transition-colors",
      collapsed ? "justify-center" : "gap-2 px-2",
      active
        ? "border-brand/40 bg-brand/10 text-ink"
        : "border-transparent text-ink-muted hover:bg-line/50 hover:text-ink",
    );

  return (
    // 宽度只能由 `collapsed` 决定：让它跟着内容走，空间一多主区就被挤窄。
    // 左栏压在 Peco 抽屉上：抽屉收起时整块移出视口，它的右半正好落在左栏位置，
    // 而左栏是抽屉的滑动起点，被盖住就看不见开关了。
    <aside
      className={cn(
        "relative z-50 flex h-full shrink-0 flex-col overflow-hidden border-r border-line bg-rail transition-[width] duration-300 ease-out",
        collapsed ? "w-16" : "w-52",
      )}
    >
      <div className="flex h-11 shrink-0 items-center justify-center border-b border-line">
        {/* 品牌行是抽屉开关；工作空间名由下方 Peco 行承担。 */}
        <button
          type="button"
          onClick={toggleDock}
          aria-label="Peco 对话"
          title="Peco 对话"
          aria-expanded={dockOpen}
          aria-controls="peco-dock"
          className={cn(
            "grid size-8 place-items-center rounded-lg border transition-colors",
            dockOpen
              ? "border-brand/40 bg-brand/10"
              : "border-transparent hover:bg-line/50",
          )}
        >
          <BrandMark size={24} />
        </button>
      </div>

      <div
        className="min-h-0 flex-1 overflow-y-auto pt-1"
        onKeyDown={moveFocus}
      >
        {peco && (
          // `button` 的 width:auto 是 fit-content，满宽要靠块级容器；否则它比空间行窄一截。
          <div className="mx-2">
            <button
              type="button"
              onClick={openPeco}
              aria-current={routeWsId === peco.ws_id ? "true" : undefined}
              aria-label="Peco"
              title="Peco"
              data-space-row=""
              className={cn(
                "flex h-8 w-full items-center rounded-lg border font-normal transition-colors",
                collapsed ? "justify-center" : "gap-2 px-2",
                routeWsId === peco.ws_id
                  ? "border-brand/40 bg-brand/10 text-ink"
                  : "border-transparent text-ink-muted hover:bg-line/50 hover:text-ink",
              )}
            >
              <UserCog className="h-4 w-4 shrink-0" />
              {!collapsed && (
                <span className="min-w-0 truncate text-body">Peco</span>
              )}
            </button>
          </div>
        )}

        <div className="mx-2 my-1 h-px bg-line" />

        {rows.map((space) => (
          <OpenSpaceRow
            key={space.ws_id}
            space={space}
            collapsed={collapsed}
            active={routeWsId === space.ws_id}
            recent={activeWsId === space.ws_id}
            onSelect={() => activateSpace(space.ws_id)}
            onRemove={() => removeSpace(space.ws_id)}
          />
        ))}
        {rows.length > 0 && <div className="mx-2 my-1 h-px bg-line" />}

        {/* 栏目入口在收起态只留图标，文字由 title / aria-label 承担；
            「工作室」同时是打开空间的入口。 */}
        <Link
          to="/spaces"
          title={collapsed ? "工作室" : undefined}
          aria-label={collapsed ? "工作室" : undefined}
          aria-current={onSpacesPage ? "page" : undefined}
          className={navRowClass(onSpacesPage)}
        >
          <LayoutGrid className="h-4 w-4 shrink-0" />
          {!collapsed && "工作室"}
        </Link>
        <Link
          to="/teams"
          title={collapsed ? "团队" : undefined}
          aria-label={collapsed ? "团队" : undefined}
          aria-current={onTeamsPage ? "page" : undefined}
          className={navRowClass(onTeamsPage)}
        >
          <Users className="h-4 w-4 shrink-0" />
          {!collapsed && (
            <>
              团队
              <span className="ml-auto tabular-nums text-2xs text-ink-faint">
                {teams?.length ?? 0}
              </span>
            </>
          )}
        </Link>
        <Link
          to="/results"
          title={collapsed ? "成果" : undefined}
          aria-label={collapsed ? "成果" : undefined}
          aria-current={onResultsPage ? "page" : undefined}
          className={navRowClass(onResultsPage)}
        >
          <Package className="h-4 w-4 shrink-0" />
          {!collapsed && (
            <>
              成果
              <span
                title={`今日 ${todayResults}`}
                className="ml-auto tabular-nums text-caption font-medium text-ink-faint"
              >
                {todayResults}
              </span>
            </>
          )}
        </Link>
        <Link
          to="/schedules"
          title={collapsed ? "定时任务" : undefined}
          aria-label={collapsed ? "定时任务" : undefined}
          aria-current={onSchedulesPage ? "page" : undefined}
          className={navRowClass(onSchedulesPage)}
        >
          <Clock className="h-4 w-4 shrink-0" />
          {!collapsed && "定时任务"}
        </Link>
      </div>

      {/* 底栏：展开态主题与设置靠左、折叠靠右；收起态只留最底部的折叠按钮。 */}
      {(!collapsed || collapsible) && (
        <div
          className={cn(
            "shrink-0 border-t border-line",
            collapsed
              ? "flex items-center justify-center py-1"
              : "flex h-11 items-center px-2",
          )}
        >
          {!collapsed && (
            <>
              <button
                type="button"
                onClick={() => setTheme(theme === "dark" ? "light" : "dark")}
                aria-label={theme === "dark" ? "切到亮色" : "切到暗色"}
                className="grid h-6 w-6 place-items-center rounded-md text-ink-faint transition-colors hover:bg-line hover:text-ink"
              >
                {theme === "dark" ? (
                  <Sun className="h-4 w-4" />
                ) : (
                  <Moon className="h-4 w-4" />
                )}
              </button>
              <Link
                to="/settings"
                title="设置"
                aria-label="设置"
                aria-current={onSettingsPage ? "page" : undefined}
                className={cn(
                  "ml-1 grid h-6 w-6 place-items-center rounded-md transition-colors hover:bg-line",
                  onSettingsPage
                    ? "bg-brand/10 text-ink"
                    : "text-ink-faint hover:text-ink",
                )}
              >
                <Settings className="h-4 w-4" />
              </Link>
            </>
          )}
          {collapsible && (
            <button
              type="button"
              onClick={() => setRailOpen(!railOpen)}
              aria-label={railOpen ? "折叠左栏" : "展开左栏"}
              className={cn(
                "grid h-6 w-6 place-items-center rounded-md text-ink-faint transition-colors hover:bg-line hover:text-ink",
                !collapsed && "ml-auto",
              )}
            >
              {railOpen ? (
                <PanelLeftClose className="h-4 w-4" />
              ) : (
                <PanelLeftOpen className="h-4 w-4" />
              )}
            </button>
          )}
        </div>
      )}
    </aside>
  );
}
