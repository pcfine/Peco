// 右栏的模式与收起态按空间各记一份：回到某个空间时看到的是上次离开时的样子。
//
// 只存 `{ mode, collapsed }` 两个字段，不进 query 缓存 —— 它是纯 UI 偏好，
// 与 daemon 的状态无关，刷新页面也不该丢。

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

export interface SpacePanelControls {
  mode: SpacePanelMode;
  collapsed: boolean;
  /** 点任一模式都是「展开并看这个模式」：收起时先展开再切。 */
  selectMode: (mode: SpacePanelMode) => void;
  toggleCollapsed: () => void;
}

export function useSpacePanelState(
  wsId: string,
  variant: SpacePanelVariant = "space",
): SpacePanelControls {
  const [stored, setStored] = useState(() => ({
    wsId,
    panel: readPanel(wsId, variant),
  }));

  // 换空间不重挂页面：在渲染期把状态换成新空间的存档，避免先渲染一帧
  // 上一个空间的面板。
  if (stored.wsId !== wsId)
    setStored({ wsId, panel: readPanel(wsId, variant) });
  const panel = stored.wsId === wsId ? stored.panel : readPanel(wsId, variant);

  const write = useCallback(
    (next: SpacePanelState) => {
      setStored({ wsId, panel: next });
      savePanel(wsId, next);
    },
    [wsId],
  );

  return {
    mode: panel.mode,
    collapsed: panel.collapsed,
    selectMode: useCallback(
      (mode: SpacePanelMode) => write({ mode, collapsed: false }),
      [write],
    ),
    toggleCollapsed: useCallback(
      () => write({ mode: panel.mode, collapsed: !panel.collapsed }),
      [write, panel.mode, panel.collapsed],
    ),
  };
}
