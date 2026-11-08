// 窄屏（work < 824）的右栏：内联一列会把聊天列压到读不了，改成覆盖式抽屉。
//
// 内容由调用方给同一个 `SpaceSidePanel` —— 抽屉只是右栏的另一种形态，不是另一套面板。
// 抽屉盖住了整条页头，所以关闭按钮必须在抽屉里；页头的 chevron 是开的入口。

import { useEffect, type ReactNode } from "react";
import { X } from "lucide-react";
import { Sheet, SheetContent } from "@/components/ui/sheet";

/**
 * 打开时主区不可交互：`main` 在壳层，不在本组件的树下，只能直接改属性。
 * radix 只对兄弟节点做 `aria-hidden`，读屏挡住了，键盘 Tab 还能进去。
 */
function useInertMain(active: boolean) {
  useEffect(() => {
    const main = document.getElementById("main");
    if (!active || !main) return;
    main.setAttribute("inert", "");
    return () => main.removeAttribute("inert");
  }, [active]);
}

interface SpacePanelDrawerProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 触发按钮：radix 没有 Trigger 可找，关闭后由它把焦点还回去。 */
  returnFocusTo: React.RefObject<HTMLButtonElement | null>;
  children: ReactNode;
}

export function SpacePanelDrawer({
  open,
  onOpenChange,
  returnFocusTo,
  children,
}: SpacePanelDrawerProps) {
  useInertMain(open);

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        side="right"
        aria-label="空间右栏"
        showCloseButton={false}
        className="w-80 gap-0 overscroll-contain border-l border-line bg-canvas p-0"
        onCloseAutoFocus={(event) => {
          event.preventDefault();
          returnFocusTo.current?.focus();
        }}
      >
        {/* 抽屉盖住页头，只能自带关闭按钮；这里不是右栏的头部，不摆模式控件。 */}
        <div className="flex h-8 shrink-0 items-center justify-end px-1">
          <button
            type="button"
            aria-label="关闭右栏"
            onClick={() => onOpenChange(false)}
            className="grid size-6 shrink-0 place-items-center rounded-[4px] text-ink-faint hover:bg-line hover:text-ink"
          >
            <X aria-hidden="true" className="h-4 w-4" />
          </button>
        </div>
        <div className="min-h-0 flex-1">{children}</div>
      </SheetContent>
    </Sheet>
  );
}
