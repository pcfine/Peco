/**
 * 时间戳常驻 DOM、hover 才可见：`absolute top-full` 让它在气泡下方呈现而不
 * 参与布局（hover 前后零位移）。刻意不加过渡动画 —— 需求只要「出现」与
 * 「不显示」两种状态，带过渡反而让计算样式出现中间值。
 */
export const MESSAGE_TIME_CLASS =
  "pointer-events-none select-none absolute top-full z-10 mt-0.5 text-[10px] leading-none text-muted-foreground opacity-0 group-hover:opacity-100";
