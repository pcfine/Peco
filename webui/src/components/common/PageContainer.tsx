import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

/**
 * 内容宽度档位。
 *
 * - `wide`：列表、表格、DAG、编辑器——宽度直接换成信息密度，越宽越好。
 * - `form`：多段表单——够放下双栏分段与双列表单，但不再往上拉，
 *   因为输入框的可用宽度超过 ~800px 后阅读成本反而上升。
 *
 * 会话正文（ChatView）刻意不接入：行宽需保持在 80 字符以内。
 */
export type PageWidth = "wide" | "form";

const WIDTH_CLASS: Record<PageWidth, string> = {
  wide: "max-w-[2200px]",
  form: "max-w-[1600px]",
};

interface PageContainerProps {
  children: ReactNode;
  width?: PageWidth;
  className?: string;
}

/**
 * 页面外框：统一内容宽度，并建立容器查询上下文。
 *
 * 宽度按「实际可用宽度」判定而非视口宽度——侧栏展开/收起相差 176px，
 * 用视口断点会在 1280–1600 这一段判断失准（同一视口下内容宽度可以差一整列）。
 */
export function PageContainer({
  children,
  width = "wide",
  className,
}: PageContainerProps) {
  return (
    <div
      className={cn(
        "@container mx-auto w-full space-y-4",
        WIDTH_CLASS[width],
        className,
      )}
    >
      {children}
    </div>
  );
}
