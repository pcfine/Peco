// 右栏的模式与收起态按空间各记一份：回到某个空间时看到的是上次离开时的样子。
//
// 只存 `{ mode, collapsed }` 两个字段，不进 query 缓存 —— 它是纯 UI 偏好，
// 与 daemon 的状态无关，刷新页面也不该丢。
//
// 模式同时能由 URL 的 `?panel=` 给出：它是「正在看哪一栏」，值得被分享和深链，
// 于是压过存档；`collapsed` 是偏好而不是可分享状态，只走存档。

import { useCallback, useState } from "react";

export type SpacePanelMode = "overview" | "files" | "tasks" | "memory";

/** Peco 页与空间页共用一个右栏，可选模式与默认落点不同。 */
export type SpacePanelVariant = "space" | "peco";

/** 按钮顺序：任务与记忆是「去看发生了什么」；总览只属于 Peco，所以排在最后。 */
export const SPACE_PANEL_MODES: Record<
  SpacePanelVariant,
  readonly SpacePanelMode[]
> = {
  space: ["tasks", "memory", "files"],
  peco: ["tasks", "memory", "files", "overview"],
};

export const SPACE_PANEL_LABELS: Record<SpacePanelMode, string> = {
  overview: "总览",
  files: "文件",
  tasks: "任务",
  memory: "记忆",
};

export interface SpacePanelState {
  mode: SpacePanelMode;
  collapsed: boolean;
}

function defaultPanel(variant: SpacePanelVariant): SpacePanelState {
  return { mode: variant === "peco" ? "overview" : "files", collapsed: false };
}

function storageKey(wsId: string): string {
  return `peco-webui:space-panel:${wsId}`;
}

function isModeOf(
  variant: SpacePanelVariant,
  value: unknown,
): value is SpacePanelMode {
  return SPACE_PANEL_MODES[variant].includes(value as SpacePanelMode);
}

/** 存档读不出来就用默认态：一份坏掉的偏好不该让右栏变成不可用。 */
function readPanel(wsId: string, variant: SpacePanelVariant): SpacePanelState {
  try {
    const raw = localStorage.getItem(storageKey(wsId));
    if (!raw) return defaultPanel(variant);
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null)
      return defaultPanel(variant);
    const { mode, collapsed } = parsed as {
      mode?: unknown;
      collapsed?: unknown;
    };
    const fallback = defaultPanel(variant);
    return {
      // 存档可能来自另一个变体或旧版本：这个变体不认的模式回默认落点。
      mode: isModeOf(variant, mode) ? mode : fallback.mode,
      collapsed:
        typeof collapsed === "boolean" ? collapsed : fallback.collapsed,
    };
  } catch {
    // 隐私模式会禁用 localStorage；旧版本写下的 JSON 也可能已经解析不了。
    return defaultPanel(variant);
  }
}

function savePanel(wsId: string, panel: SpacePanelState): void {
  try {
    localStorage.setItem(storageKey(wsId), JSON.stringify(panel));
  } catch {
    // 存不下只意味着下次回到默认态，本次使用不受影响。
  }
}

export interface SpacePanelOptions {
  /** work < 824 时右栏改为抽屉：开关是瞬时态，不改也不落存档里的 `collapsed`。 */
  drawer?: boolean;
  /** URL 的 `?panel=` 原值：合法时优先于存档，让「正在看哪一栏」可分享、可深链。 */
  modeFromUrl?: string | null;
  /** 模式变了才写 URL —— 幂等的点击不该多一次 `replaceState`。 */
  onModeChange?: (mode: SpacePanelMode) => void;
}

export interface SpacePanelControls {
  mode: SpacePanelMode;
  /** 内联态的收起偏好；抽屉态恒等于存档里的值，不参与渲染。 */
  collapsed: boolean;
  /** 抽屉是否打开；内联态恒为 false。 */
  drawerOpen: boolean;
  /** 点任一模式都是「展开并看这个模式」：收起时先展开再切，抽屉态则打开抽屉。 */
  selectMode: (mode: SpacePanelMode) => void;
  toggleCollapsed: () => void;
  setDrawerOpen: (open: boolean) => void;
}

export function useSpacePanelState(
  wsId: string,
  variant: SpacePanelVariant = "space",
  options: SpacePanelOptions = {},
): SpacePanelControls {
  const { drawer = false, modeFromUrl = null, onModeChange } = options;

  const [stored, setStored] = useState(() => ({
    wsId,
    panel: readPanel(wsId, variant),
  }));
  // 抽屉的开关不落存档：视口变宽后再变窄，不该自动弹出来。
  const [drawerOpen, setDrawerOpen] = useState(false);

  // 换空间不重挂页面：在渲染期把状态换成新空间的存档，避免先渲染一帧
  // 上一个空间的面板。
  if (stored.wsId !== wsId)
    setStored({ wsId, panel: readPanel(wsId, variant) });
  const panel = stored.wsId === wsId ? stored.panel : readPanel(wsId, variant);

  // 视口放宽回到内联态后，别把「抽屉开着」留到下一次变窄。
  if (!drawer && drawerOpen) setDrawerOpen(false);

  // URL 里的模式压过存档；参数值不属于这个变体（`overview` 之于空间页、
  // 手改坏的串）就当没给，回落到存档与默认。
  const mode = isModeOf(variant, modeFromUrl) ? modeFromUrl : panel.mode;

  const write = useCallback(
    (next: SpacePanelState) => {
      setStored({ wsId, panel: next });
      savePanel(wsId, next);
    },
    [wsId],
  );

  const selectMode = useCallback(
    (next: SpacePanelMode) => {
      // 抽屉态只改模式：`collapsed` 原样写回，视口变宽后仍按存档恢复。
      write({ mode: next, collapsed: drawer ? panel.collapsed : false });
      if (drawer) setDrawerOpen(true);
      if (next !== mode) onModeChange?.(next);
    },
    [write, drawer, panel.collapsed, mode, onModeChange],
  );

  return {
    mode,
    collapsed: panel.collapsed,
    drawerOpen: drawer && drawerOpen,
    selectMode,
    toggleCollapsed: useCallback(
      () => write({ mode, collapsed: !panel.collapsed }),
      [write, mode, panel.collapsed],
    ),
    setDrawerOpen,
  };
}
