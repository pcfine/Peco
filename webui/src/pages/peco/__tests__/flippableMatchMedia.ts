// 可翻转的 matchMedia：断点形态的用例要在同一次挂载内改变视口宽度。
//
// `useMediaQuery` 会调两次 `matchMedia`（初值 + effect 订阅），两次拿到的对象必须
// 共享同一 query 的订阅集合，否则翻转时只有其中一条链路被通知到。
// 宽度是唯一真相源：突变的断点数字（1440 → 1280）会被正则读出来，用例据以红。

import { act } from "react";
import { vi } from "vitest";

type ChangeListener = (event: { matches: boolean }) => void;

export interface FlippableViewport {
  /** 改宽度并同步派发 change，包在 `act` 里以免 React 记一次未批处理的更新。 */
  setViewportWidth(width: number): void;
}

export function installFlippableMatchMedia(
  initialWidth: number,
): FlippableViewport {
  let viewportWidth = initialWidth;
  const subscribers = new Map<string, Set<ChangeListener>>();

  const matchesOf = (query: string) => {
    const minWidth = /min-width:\s*(\d+)px/.exec(query);
    return minWidth ? viewportWidth >= Number(minWidth[1]) : false;
  };

  vi.stubGlobal("matchMedia", (query: string) => {
    const listeners = subscribers.get(query) ?? new Set<ChangeListener>();
    subscribers.set(query, listeners);
    return {
      media: query,
      get matches() {
        return matchesOf(query);
      },
      onchange: null,
      addEventListener: (type: string, listener: ChangeListener) => {
        if (type === "change") listeners.add(listener);
      },
      removeEventListener: (type: string, listener: ChangeListener) => {
        if (type === "change") listeners.delete(listener);
      },
      addListener: (listener: ChangeListener) => listeners.add(listener),
      removeListener: (listener: ChangeListener) => listeners.delete(listener),
      dispatchEvent: () => false,
    } as unknown as MediaQueryList;
  });

  return {
    setViewportWidth(width: number) {
      viewportWidth = width;
      act(() => {
        for (const [query, listeners] of subscribers) {
          for (const listener of listeners) {
            listener({ matches: matchesOf(query) });
          }
        }
      });
    },
  };
}
