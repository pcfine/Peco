// 返回上一级的图标按钮都走这里：同一个 < 形 chevron、同一档尺寸。
// 文字型返回（如「返回工作室」链接）指向的是去处而不是退栈，不走这里。

import { ChevronLeft } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

interface BackButtonProps {
  /** 无障碍名与 tooltip 用同一个值。 */
  label: string;
  onClick: () => void;
  className?: string;
}

export function BackButton({ label, onClick, className }: BackButtonProps) {
  return (
    <Button
      variant="ghost"
      size="icon-sm"
      className={cn("text-ink-muted", className)}
      aria-label={label}
      title={label}
      onClick={onClick}
    >
      <ChevronLeft className="size-5" aria-hidden="true" />
    </Button>
  );
}
