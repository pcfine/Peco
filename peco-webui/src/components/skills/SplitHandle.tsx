// 分栏分隔线：1px 视觉线撑在 6px 热区里，可拖拽、可键盘、可双击复位。
//
// 6px 只够看着细，指点设备抓不住：伪元素在两侧各扩 12px 当热区，布局占位与视觉线
// 都不动，指针事件仍派发给 handle 本身。焦点环走全站统一的 :focus-visible。
//
// 不用 setPointerCapture：拖拽过程中指针可能离开热区，捕获只是省掉 window 监听，
// 而在部分环境（含 jsdom）里没有这个方法，会直接抛错。

import { useEffect, useRef } from "react";

/** 方向键的调整步长：一次 16px 约等于两个字符宽，够细也够快。 */
const KEY_STEP = 16;

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

interface SplitHandleProps {
  /** 当前（生效的）预览栏宽度，同时作为 aria-valuenow。 */
  width: number;
  min: number;
  max: number;
  defaultWidth: number;
  onChange: (next: number) => void;
}

export function SplitHandle({
  width,
  min,
  max,
  defaultWidth,
  onChange,
}: SplitHandleProps) {
  /** 拖拽期间要清理的东西：松手、失焦、卸载都走同一个函数。 */
  const endDragRef = useRef<(() => void) | null>(null);

  // 拖动中被卸载（切视图、切文件）也要把 window 监听与 body 的选区抑制收回来。
  useEffect(() => () => endDragRef.current?.(), []);

  function onPointerDown(event: React.PointerEvent<HTMLDivElement>) {
    if (event.button !== 0) return;
    event.preventDefault();
    endDragRef.current?.();

    const startX = event.clientX;
    const startWidth = width;
    const previousSelect = document.body.style.userSelect;
    // 拖动时指针会扫过两侧正文，不关掉选区就会顺手选中一大片文字。
    document.body.style.userSelect = "none";

    const onMove = (moveEvent: PointerEvent) => {
      // 预览栏在右侧：指针左移等于加宽预览。
      onChange(clamp(startWidth - (moveEvent.clientX - startX), min, max));
    };
    const finish = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("blur", finish);
      document.body.style.userSelect = previousSelect;
      endDragRef.current = null;
    };

    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", finish);
    window.addEventListener("blur", finish);
    endDragRef.current = finish;
  }

  function onKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
    const next: Record<string, number> = {
      ArrowLeft: width + KEY_STEP,
      ArrowRight: width - KEY_STEP,
      Home: min,
      End: max,
    };
    const target = next[event.key];
    if (target === undefined) return;
    event.preventDefault();
    onChange(clamp(target, min, max));
  }

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      tabIndex={0}
      aria-label="调整源码与预览宽度"
      aria-valuenow={width}
      aria-valuemin={min}
      aria-valuemax={max}
      aria-valuetext={`预览栏 ${width}px`}
      className="group relative w-1.5 shrink-0 cursor-col-resize touch-none select-none after:absolute after:inset-y-0 after:-left-3 after:-right-3 after:content-['']"
      onPointerDown={onPointerDown}
      onKeyDown={onKeyDown}
      onDoubleClick={() => onChange(clamp(defaultWidth, min, max))}
    >
      <div className="pointer-events-none absolute inset-y-0 left-1/2 w-px bg-line transition-colors group-hover:bg-primary/60 group-focus-visible:bg-primary" />
    </div>
  );
}
