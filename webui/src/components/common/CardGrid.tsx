import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

interface CardGridProps {
  children: ReactNode;
  className?: string;
}

/**
 * 卡片栅格：列数随容器宽度增长，1 → 2 → 3 → 4 → 5 列。
 *
 * 阈值按「单卡不小于 ~330px」反推，保证卡片不会因为多一列而挤到读不清。
 * 1920×1080（侧栏展开、容器 1616px）落在 4 列。
 * 需置于 PageContainer 内（提供 @container 上下文）。
 */
export function CardGrid({ children, className }: CardGridProps) {
  return (
    <div
      className={cn(
        "grid grid-cols-1 gap-3",
        "@min-[660px]:grid-cols-2 @min-[660px]:gap-4",
        "@min-[1040px]:grid-cols-3",
        "@min-[1480px]:grid-cols-4",
        "@min-[1900px]:grid-cols-5",
        className,
      )}
    >
      {children}
    </div>
  );
}
