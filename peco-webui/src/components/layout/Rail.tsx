// 左栏：品牌 / 总管 / 空间总线 / 跨空间索引 / 底栏。
// 它既是导航又是「当前空间」切换器 —— 所以空间列表就是导航本体，不做二级下拉。

import { useState, type KeyboardEvent } from "react";
import { Link } from "react-router-dom";
import { PanelLeftClose, PanelLeftOpen, Plus, Settings } from "lucide-react";
import { BrandMark } from "@/components/common/BrandMark";
import { SpaceBusRow, spaceBusModel } from "@/components/layout/SpaceBusRow";
import { usePeco } from "@/queries/peco";
import { useSpaces } from "@/queries/spaces";
import { useRecentResults } from "@/queries/tasks";
import { useTeams } from "@/queries/teams";
import { useSpaceStore } from "@/stores/spaceStore";
import { useSwitchSpace } from "@/hooks/useSwitchSpace";
import { useThemeStore } from "@/stores/themeStore";
import { useUiStore } from "@/stores/uiStore";
import { isToday } from "@/lib/relative";
import { cn } from "@/lib/utils";

/** 总线上常显的空间行数；其余折成「+N 个」。 */
const VISIBLE_SPACES = 8;

export function Rail({ collapsed = false }: { collapsed?: boolean }) {
  const [showAll, setShowAll] = useState(false);
  const railOpen = useUiStore((state) => state.railOpen);
  const setRailOpen = useUiStore((state) => state.setRailOpen);
  const theme = useThemeStore((state) => state.theme);
  const setTheme = useThemeStore((state) => state.setTheme);
  const currentWsId = useSpaceStore((state) => state.currentWsId);
  const switchSpace = useSwitchSpace();

  const { data: peco } = usePeco();
  const { data: spaces } = useSpaces();
  const { data: teams } = useTeams();
  const { data: results } = useRecentResults();

  const openSpaces = (spaces ?? []).filter((space) => space.status === "open");
  // 总管固定在首行，不进空间总线列表：它不属于「执行空间」那一段。
  const busSpaces = (spaces ?? []).filter(
    (space) => space.ws_id !== peco?.ws_id,
  );
  const shown = showAll ? busSpaces : busSpaces.slice(0, VISIBLE_SPACES);
  const hiddenCount = busSpaces.length - shown.length;
  const todayResults = (results ?? []).filter((task) =>
    isToday(task.terminal_at ?? task.created_at),
  ).length;
  const pecoSpace = (spaces ?? []).find((space) => space.ws_id === peco?.ws_id);

  // 总线行之间 ↑/↓ 移动焦点：行本身是按钮，Enter/Space 天然生效。
  const moveFocus = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    const rows = Array.from(
      event.currentTarget.querySelectorAll<HTMLElement>("[data-space-row]"),
    );
    const index = rows.indexOf(document.activeElement as HTMLElement);
    if (index === -1) return;
    event.preventDefault();
    const next = event.key === "ArrowDown" ? index + 1 : index - 1;
    rows[Math.min(Math.max(next, 0), rows.length - 1)]?.focus();
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
          <SpaceBusRow
            model={{
              wsId: peco.ws_id,
              name: "Peco",
              teamId: peco.team_id,
              inflight: pecoSpace?.inflight_dispatch ?? 0,
            }}
            collapsed={collapsed}
            active={currentWsId === peco.ws_id}
            onSelect={() => switchSpace(peco.ws_id)}
          />
        )}

        <div className="mx-2 my-1 h-px bg-line" />

        {!collapsed && (
          <Link
            to="/spaces"
            className="flex h-8 items-center gap-2 pl-3 pr-2 text-xs font-semibold text-ink-muted hover:text-ink"
          >
            空间
            <span className="ml-auto tabular-nums text-2xs text-ink-faint">
              进行中 {openSpaces.length}
            </span>
          </Link>
        )}
        {shown.map((space) => (
          <SpaceBusRow
            key={space.ws_id}
            model={spaceBusModel(space)}
            collapsed={collapsed}
            active={currentWsId === space.ws_id}
            onSelect={() => switchSpace(space.ws_id)}
          />
        ))}
        {!collapsed && hiddenCount > 0 && (
          <button
            type="button"
            onClick={() => setShowAll(true)}
            className="flex h-8 w-full items-center pl-3 pr-2 text-left text-2xs text-ink-faint hover:text-ink"
          >
            +{hiddenCount} 个空间 →
          </button>
        )}
        {!collapsed && (
          <Link
            to="/spaces/new"
            className="flex h-8 w-full items-center gap-1.5 pl-3 pr-2 text-2xs text-ink-faint hover:text-ink"
          >
            <Plus className="h-3 w-3" />
            新建空间
          </Link>
        )}

        <div className="mx-2 my-1 h-px bg-line" />

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

      <div
        className={cn(
          "flex h-11 shrink-0 items-center border-t border-line px-2",
          collapsed && "justify-center",
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
        {!collapsed && (
          <button
            type="button"
            disabled
            title="下一版"
            aria-label="设置（下一版）"
            className="ml-1 grid h-6 w-6 cursor-not-allowed place-items-center rounded-[4px] text-ink-faint opacity-60"
          >
            <Settings className="h-4 w-4" />
          </button>
        )}
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
