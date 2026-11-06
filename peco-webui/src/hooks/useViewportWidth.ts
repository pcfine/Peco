import { useEffect, useState } from "react";

/**
 * 窗口宽度。dock 的形态由「右列宽 = viewport − rail」判定（不是容器宽），
 * 所以这里读视口：容器宽已经把 dock 自己算进去了，用它判定会自指。
 */
export function useViewportWidth(): number {
  const [width, setWidth] = useState(() => window.innerWidth);
  useEffect(() => {
    const onResize = () => setWidth(window.innerWidth);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);
  return width;
}
