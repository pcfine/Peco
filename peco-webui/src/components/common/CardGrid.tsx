// 资源面板共用的卡片栅格：列数只看面板宽度（`@container/team`），与视口无关 ——
// 同一份栅格在团队页与 Peco 翻转面里都要成立。
//
// 阈值按「卡不小于 260px」反推：`@container/team` 量的是含左侧分类栏的整块，
// 所以每加一列要先扣掉 176px 导航与 24px 内边距。上限 3 列 —— 容器被
// `max-w-wide`(1200) 封顶，第 4 列在 1440 上只剩 237px。

import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

interface CardGridProps {
  children: ReactNode;
  className?: string;
}

export function CardGrid({ children, className }: CardGridProps) {
  return (
    <div
      className={cn(
        "grid grid-cols-1 gap-3 @min-[720px]/team:grid-cols-2 @min-[1060px]/team:grid-cols-3",
        className,
      )}
    >
      {children}
    </div>
  );
}

interface EntityCardProps {
  /**
   * 不给就是纯展示卡：渲染成 `div`，没有 hover 边框变化，也不进 tab 序 ——
   * 一类资源没有详情可进时，卡不该长得像是能点的。
   */
  onClick?: () => void;
  children: ReactNode;
  /**
   * 卡内操作按钮。渲染成卡本体的兄弟节点：`button` 不能嵌 `button`，
   * 而 `div role="button"` 里放真按钮会让内层控件在无障碍树里丢掉「按钮」语义。
   */
  actions?: ReactNode;
  className?: string;
}

export function EntityCard({
  onClick,
  children,
  actions,
  className,
}: EntityCardProps) {
  const bodyClass = cn(
    "flex min-h-[132px] w-full flex-col gap-2 rounded-[6px] bg-side p-3 text-left transition-colors",
    onClick &&
      "hover:bg-side/80 hover:ring-1 hover:ring-brand/40 focus-visible:ring-2 focus-visible:ring-brand",
    className,
  );

  return (
    // `group` 挂在外层：卡本体与操作区是两个兄弟，只有共同的父才能同时响应两者。
    <div className="group relative min-w-0">
      {onClick ? (
        <button type="button" onClick={onClick} className={bodyClass}>
          {children}
        </button>
      ) : (
        <div className={bodyClass}>{children}</div>
      )}
      {actions && (
        <div className="pointer-events-none absolute top-2 right-2 flex gap-1 opacity-0 transition-opacity group-focus-within:pointer-events-auto group-focus-within:opacity-100 group-hover:pointer-events-auto group-hover:opacity-100 pointer-coarse:pointer-events-auto pointer-coarse:opacity-100">
          {actions}
        </div>
      )}
    </div>
  );
}
