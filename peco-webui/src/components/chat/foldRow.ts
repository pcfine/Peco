// 折叠行外壳 —— 推理行与工具行共用同一串类，两行只在子元素与展开体上不同。
// 单点定义是硬要求：分处两个组件各抄一份，任何一处手滑都会让两行高度/内距漂开。

export const FOLD_ROW =
  "group flex w-full cursor-pointer items-center gap-1 rounded-md px-2 py-1.5 text-left text-xs hover:bg-tool-row-hover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50";

// 箭头旋转走 rotate 而不是 scale/translate：只改 rotate 属性，不参与布局，标签不位移。
export const CHEVRON =
  "h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform duration-150 group-data-[state=open]:rotate-90 motion-reduce:transition-none";
