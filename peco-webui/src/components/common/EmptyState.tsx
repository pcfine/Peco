// 空态统一形：一句事实 + 一个可选动作。文案不带句末句号。左对齐、不居中，也不给插画位。

import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

interface EmptyStateProps {
  title: string;
  action?: ReactNode;
  className?: string;
}

export function EmptyState({ title, action, className }: EmptyStateProps) {
  return (
    <div className={cn("max-w-narrow space-y-1 py-2", className)}>
      <p className="text-sm text-ink-muted">{title}</p>
      {action}
    </div>
  );
}
