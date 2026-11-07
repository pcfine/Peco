// 右栏的模式与收起态按空间各记一份：回到某个空间时看到的是上次离开时的样子。
//
// 只存 `{ mode, collapsed }` 两个字段，不进 query 缓存 —— 它是纯 UI 偏好，
// 与 daemon 的状态无关，刷新页面也不该丢。

import { useCallback, useState } from "react";

export type SpacePanelMode = "files" | "tasks" | "memory";

/** 按钮顺序：任务与记忆是「去看发生了什么」，文件是默认落点。 */
export const SPACE_PANEL_MODES: readonly SpacePanelMode[] = [
  "tasks",
  "memory",
  "files",
];

export const SPACE_PANEL_LABELS: Record<SpacePanelMode, string> = {
  files: "文件",
  tasks: "任务",
  memory: "记忆",
};

export interface SpacePanelState {
  mode: SpacePanelMode;
  collapsed: boolean;
}

function defaultPanel(): SpacePanelState {
  return { mode: "files", collapsed: false };
}

function storageKey(wsId: string): string {
  return `peco-webui:space-panel:${wsId}`;
}

function isMode(value: unknown): value is SpacePanelMode {
  return SPACE_PANEL_MODES.includes(value as SpacePanelMode);
}

/** 存档读不出来就用默认态：一份坏掉的偏好不该让右栏变成不可用。 */
function readPanel(wsId: string): SpacePanelState {
  try {
    const raw = localStorage.getItem(storageKey(wsId));
    if (!raw) return defaultPanel();
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return defaultPanel();
    const { mode, collapsed } = parsed as {
      mode?: unknown;
      collapsed?: unknown;
    };
    const fallback = defaultPanel();
    return {
      mode: isMode(mode) ? mode : fallback.mode,
      collapsed:
        typeof collapsed === "boolean" ? collapsed : fallback.collapsed,
    };
  } catch {
    // 隐私模式会禁用 localStorage；旧版本写下的 JSON 也可能已经解析不了。
    return defaultPanel();
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

export function useSpacePanelState(wsId: string): SpacePanelControls {
  const [stored, setStored] = useState(() => ({
    wsId,
    panel: readPanel(wsId),
  }));

  // 换空间不重挂页面：在渲染期把状态换成新空间的存档，避免先渲染一帧
  // 上一个空间的面板。
  if (stored.wsId !== wsId) setStored({ wsId, panel: readPanel(wsId) });
  const panel = stored.wsId === wsId ? stored.panel : readPanel(wsId);

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
