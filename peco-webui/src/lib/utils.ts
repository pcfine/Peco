import { clsx, type ClassValue } from "clsx";
import { extendTailwindMerge } from "tailwind-merge";

// tailwind-merge 只认识 Tailwind 内置的 text-xs / text-sm / text-base 这些字号类；
// v3 引入的语义字号（micro / caption / body / reading / heading / display）不在它的
// 规则里，会被当成「文字颜色」。于是 `cn("text-caption", "text-ink")` 会把字号静默
// 丢掉、只剩颜色 —— 页面看起来「字号没生效」，其实类根本没挂上去。
// 把六个语义名登记进 font-size 组，字号与颜色才各归其位。
const twMerge = extendTailwindMerge({
  extend: {
    classGroups: {
      "font-size": [
        { text: ["micro", "caption", "body", "reading", "heading", "display"] },
      ],
    },
  },
});

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
