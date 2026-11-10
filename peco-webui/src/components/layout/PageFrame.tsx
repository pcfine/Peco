// 一级栏目页的统一页框：全屏贴左，标题行固定，正文自己滚。

import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

interface PageFrameProps {
  title: string;
  /** 标题行右侧的动作区。 */
  actions?: ReactNode;
  /** 内容的可读宽度；缺省吃满整宽。 */
  contentWidth?: "full" | "mid" | "narrow";
  children: ReactNode;
}

const CONTENT_WIDTH = {
  full: "",
  mid: "max-w-mid",
  narrow: "max-w-narrow",
} as const;

export function PageFrame({
  title,
  actions,
  contentWidth = "full",
  children,
}: PageFrameProps) {
  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="flex h-11 shrink-0 items-center gap-2 border-b border-line px-4 @min-[900px]/shell:px-6">
        <h1 className="min-w-0 truncate text-xl font-semibold text-ink">
          {title}
        </h1>
        {actions !== undefined && (
          <div className="ml-auto flex shrink-0 items-center gap-2">
            {actions}
          </div>
        )}
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto px-4 pb-6 @min-[900px]/shell:px-6">
        <div className={cn("pt-4", CONTENT_WIDTH[contentWidth])}>
          {children}
        </div>
      </div>
    </div>
  );
}
