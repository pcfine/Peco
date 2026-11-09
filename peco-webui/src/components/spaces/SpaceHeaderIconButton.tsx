// 头部只用图标的按钮：文字说明挂在 Tooltip 上，`aria-label` 与它是同一个值 ——
// 读屏用户与鼠标用户听到的是同一句话。

import type { Ref } from "react";
import type { LucideIcon } from "lucide-react";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

interface SpaceHeaderIconButtonProps {
  label: string;
  icon: LucideIcon;
  onClick: () => void;
  /** 按不动的原因；有值时替代同名的默认文案，回答「为什么点不动」。 */
  hint?: string;
  /**
   * 用 `aria-disabled` 而非 `disabled`：后者不再接收指针事件，Tooltip 就永远不出来，
   * 「为什么点不动」也就无从得知；点击的守卫放在处理器里。
   */
  disabled?: boolean;
  /** 破坏性动作只在 hover 时染红，静止态不与普通按钮抢注意力。 */
  tone?: "normal" | "danger";
  ref?: Ref<HTMLButtonElement>;
  className?: string;
}

export function SpaceHeaderIconButton({
  label,
  icon: Icon,
  onClick,
  hint,
  disabled = false,
  tone = "normal",
  ref,
  className,
}: SpaceHeaderIconButtonProps) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          ref={ref}
          type="button"
          aria-label={label}
          aria-disabled={disabled || undefined}
          onClick={disabled ? undefined : onClick}
          className={cn(
            "grid size-7 shrink-0 place-items-center rounded-[4px] text-ink-faint transition-colors duration-[120ms] hover:bg-line hover:text-ink",
            tone === "danger" && "hover:text-danger",
            disabled && "cursor-not-allowed opacity-50 hover:bg-transparent",
            className,
          )}
        >
          <Icon aria-hidden="true" className="h-4 w-4" />
        </button>
      </TooltipTrigger>
      <TooltipContent>{hint ?? label}</TooltipContent>
    </Tooltip>
  );
}
