import { useEffect, useState } from "react";

/**
 * 媒体查询判断。
 *
 * 初值同步取 `matchMedia` 的结果：先给 `false` 再在 effect 里纠正，会让首帧按
 * 窄屏排版，宽屏下打开页面能看见一次跳变。
 */
export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() => matchMedia(query).matches);

  useEffect(() => {
    const list = matchMedia(query);
    const onChange = (event: MediaQueryListEvent) => setMatches(event.matches);
    // query 变化时旧初值已过期，先对齐一次再等后续 change
    setMatches(list.matches);
    list.addEventListener("change", onChange);
    return () => list.removeEventListener("change", onChange);
  }, [query]);

  return matches;
}
