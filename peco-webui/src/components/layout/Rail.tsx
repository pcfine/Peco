// 左栏：品牌 / Peco / 已打开空间 / 工作室·团队·成果 / 固定底栏。
// 「已打开」是唯一的空间列表 —— 未打开的空间只在工作室页里，左栏不是索引。

import { type KeyboardEvent } from "react";
import { Link, useLocation, useNavigate } from "react-router-dom";
import { PanelLeftClose, PanelLeftOpen, Plus, Settings } from "lucide-react";
import { BrandMark } from "@/components/common/BrandMark";
import { OpenSpaceRow } from "@/components/layout/OpenSpaceRow";
import { useActivateSpace } from "@/hooks/useOpenSpaces";
import { usePeco } from "@/queries/peco";
import { useSpaces } from "@/queries/spaces";
import { useRecentResults } from "@/queries/tasks";
import { useTeams } from "@/queries/teams";
import { useOpenSpacesStore } from "@/stores/openSpacesStore";
import { useThemeStore } from "@/stores/themeStore";
import { useUiStore } from "@/stores/uiStore";
import { isToday } from "@/lib/relative";
import { cn } from "@/lib/utils";
import type { Space } from "@/types/daemon";

export function Rail({ collapsed = false }: { collapsed?: boolean }) {
  const railOpen = useUiStore((state) => state.railOpen);
  const setRailOpen = useUiStore((state) => state.setRailOpen);
  const theme = useThemeStore((state) => state.theme);
  const setTheme = useThemeStore((state) => state.setTheme);
  const openIds = useOpenSpacesStore((state) => state.openIds);
  const activeWsId = useOpenSpacesStore((state) => state.activeWsId);
  const removeOpen = useOpenSpacesStore((state) => state.close);
  const setActive = useOpenSpacesStore((state) => state.setActive);
  const activateSpace = useActivateSpace();
  const navigate = useNavigate();
  const { pathname, search } = useLocation();
  const onPecoConfigFace =
    pathname === "/peco" &&
    new URLSearchParams(search).get("face") === "config";

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

  // 第一下进聊天，已经在聊天面时第二下翻到能力配置，再点回来 —— Peco 是唯一
  // 一行「再点一次」有额外含义的入口；空间行仍然只切激活态。
  const openPeco = () => {
    if (!peco) return;
    setActive(peco.ws_id);
    if (pathname !== "/peco") {
      navigate("/peco");
      return;
    }
    navigate(onPecoConfigFace ? "/peco" : "/peco?face=config");
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

  return (
    // 宽度只能由 `collapsed` 决定：让它跟着内容走，空间一多主区就被挤窄。
    <aside
      className={cn(
        "flex h-full shrink-0 flex-col border-r border-line bg-canvas",
        collapsed ? "w-16" : "w-64",
      )}
    >
      {/* 收起态 64 放不下「品牌 + 间距 + 按钮」的展开式内边距：收紧留白并居中，
          否则标题行会横向溢出压到主区。 */}
      <div
        className={cn(
          "flex h-11 shrink-0 items-center",
          collapsed ? "justify-center gap-1 px-1" : "gap-2 pl-3 pr-2",
        )}
      >
        <BrandMark size={20} />
        {!collapsed && (
          <span className="text-sm font-semibold text-ink">Peco</span>
        )}
        <button
          type="button"
          onClick={() => setRailOpen(!railOpen)}
          aria-label={railOpen ? "折叠左栏" : "展开左栏"}
          className={cn(
            "grid h-6 w-6 place-items-center rounded-[4px] text-ink-faint hover:bg-line hover:text-ink",
            !collapsed && "ml-auto",
          )}
        >
          {railOpen ? (
            <PanelLeftClose className="h-4 w-4" />
          ) : (
            <PanelLeftOpen className="h-4 w-4" />
          )}
        </button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto" onKeyDown={moveFocus}>
        {peco && (
          <button
            type="button"
            onClick={openPeco}
            aria-current={activeWsId === peco.ws_id ? "true" : undefined}
            aria-label="Peco"
            title="Peco"
            data-space-row=""
            className={cn(
              "relative flex w-full items-center transition-colors duration-[120ms]",
              collapsed ? "h-8 justify-center" : "h-8 gap-2 pl-3 pr-2",
              activeWsId === peco.ws_id
                ? "bg-line/60 text-ink"
                : "text-ink-muted hover:bg-line/40",
            )}
          >
            {activeWsId === peco.ws_id && (
              <span
                data-testid="rail-active-bar"
                className="absolute inset-y-1 left-0 w-[3px] rounded-full bg-brand transition-transform duration-[120ms]"
              />
            )}
            <BrandMark size={14} />
            {!collapsed && (
              <>
                <span className="min-w-0 truncate text-sm">Peco</span>
                <span className="ml-auto shrink-0 font-mono text-2xs text-ink-faint">
                  {peco.team_id}
                </span>
              </>
            )}
          </button>
        )}

        <div className="mx-2 my-1 h-px bg-line" />

        {!collapsed && (
          <p className="flex h-7 items-center pl-3 pr-2 text-2xs font-semibold text-ink-faint">
            已打开
          </p>
        )}
        {rows.map((space) => (
          <OpenSpaceRow
            key={space.ws_id}
            space={space}
            collapsed={collapsed}
            active={activeWsId === space.ws_id}
            onSelect={() => activateSpace(space.ws_id)}
            onRemove={() => removeSpace(space.ws_id)}
          />
        ))}
        {!collapsed && rows.length === 0 && (
          <p className="pl-3 pr-2 py-1 text-2xs text-ink-faint">
            还没有打开空间。
          </p>
        )}
        <Link
          to="/spaces"
          title={collapsed ? "打开空间" : undefined}
          aria-label={collapsed ? "打开空间" : undefined}
          className={cn(
            "flex h-8 items-center gap-1.5 text-2xs text-ink-faint hover:text-ink",
            collapsed ? "justify-center" : "pl-3 pr-2",
          )}
        >
          <Plus className="h-3 w-3 shrink-0" />
          {!collapsed && "打开空间"}
        </Link>

        <div className="mx-2 my-1 h-px bg-line" />

        {!collapsed && (
          <Link
            to="/spaces"
            className="flex h-8 items-center pl-3 pr-2 text-sm text-ink-muted hover:bg-line/40 hover:text-ink"
          >
            工作室
          </Link>
        )}
        {!collapsed && (
          <Link
            to="/teams"
            className="flex h-8 items-center pl-3 pr-2 text-sm text-ink-muted hover:bg-line/40 hover:text-ink"
          >
            团队
            <span className="ml-auto tabular-nums text-2xs text-ink-faint">
              {teams?.length ?? 0}
            </span>
          </Link>
        )}
        {!collapsed && (
          <Link
            to="/results"
            className="flex h-8 w-full items-center pl-3 pr-2 text-sm text-ink-muted hover:bg-line/40 hover:text-ink"
          >
            成果
            <span className="ml-auto tabular-nums text-2xs text-ink-faint">
              今日 {todayResults}
            </span>
          </Link>
        )}
        {!collapsed && (
          <Link
            to="/schedules"
            className="flex h-8 w-full items-center pl-3 pr-2 text-sm text-ink-muted hover:bg-line/40 hover:text-ink"
          >
            定时任务
            <span className="ml-auto text-2xs text-ink-faint">未实现</span>
          </Link>
        )}
      </div>

      {/* 收起态 64 宽：两块 24 的按钮加上间距正好放得下，内边距要收窄。 */}
      <div
        className={cn(
          "flex h-11 shrink-0 items-center border-t border-line",
          collapsed ? "justify-center gap-1 px-1" : "px-2",
        )}
      >
        <button
          type="button"
          onClick={() => setTheme(theme === "dark" ? "light" : "dark")}
          aria-label={theme === "dark" ? "切到亮色" : "切到暗色"}
          className="grid h-6 w-6 place-items-center rounded-[4px] text-ink-faint hover:bg-line hover:text-ink"
        >
          <span className="text-xs">{theme === "dark" ? "◐" : "◑"}</span>
        </button>
        <Link
          to="/settings"
          title="设置"
          aria-label="设置"
          className={cn(
            "grid h-6 w-6 place-items-center rounded-[4px] text-ink-faint hover:bg-line hover:text-ink",
            !collapsed && "ml-1",
          )}
        >
          <Settings className="h-4 w-4" />
        </Link>
        {!collapsed && peco && (
          <span
            title={peco.work_dir}
            className="ml-auto max-w-[120px] truncate font-mono text-2xs text-ink-faint"
          >
            {peco.ws_id}
          </span>
        )}
      </div>
    </aside>
  );
}
