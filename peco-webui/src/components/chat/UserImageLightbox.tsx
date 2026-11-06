import type { ReactElement, RefObject } from "react";
import { X } from "lucide-react";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogTitle,
} from "@/components/ui/dialog";

export interface UserImageLightboxProps {
  /** 打开中的图片；null 表示关闭 —— 此时不挂载 portal */
  image: { src: string; label: string } | null;
  /** 触发按钮：关闭时把焦点还给它 */
  triggerElRef: RefObject<HTMLButtonElement | null>;
  onClose: () => void;
}

export function UserImageLightbox({
  image,
  triggerElRef,
  onClose,
}: UserImageLightboxProps): ReactElement | null {
  if (image === null) return null;

  return (
    <Dialog
      open
      onOpenChange={(next) => {
        if (!next) onClose();
      }}
    >
      <DialogContent
        showCloseButton={false}
        aria-describedby={undefined}
        /* ui/dialog 的默认样式是居中卡片，与铺满视口的透明层冲突，逐项覆写；
           `sm:max-w-none` 不能省 —— `sm:max-w-lg` 是另一个变体，只写 `max-w-none`
           消不掉，宽视口下容器会被卡在 512px。 */
        className="flex h-screen w-screen max-w-none items-center justify-center border-0 bg-transparent p-0 shadow-none sm:max-w-none data-[state=open]:animate-none! data-[state=closed]:animate-none!"
        onCloseAutoFocus={(event) => {
          // radix 的默认归还在 setTimeout 里、且只认它自己抓到的元素：本组件由图片按钮
          // 打开，radix 手里没有 triggerRef，必须手工还
          event.preventDefault();
          const trigger = triggerElRef.current;
          // 打开期间消息可能被重渲染掉，元素已脱离文档时 focus() 是空操作，不如显式放弃
          if (!trigger?.isConnected) return;
          trigger.focus();
        }}
        onClick={(event) => {
          // 容器铺满视口后「点空白」就是点容器自身；点图/按钮时 target 是子元素，跳过
          if (event.target === event.currentTarget) onClose();
        }}
      >
        <DialogTitle className="sr-only">{image.label}</DialogTitle>
        <img
          src={image.src}
          alt="图片预览"
          className="block h-auto w-auto max-h-[85vh] max-w-[90vw] object-contain"
        />
        <DialogClose
          aria-label="关闭图片预览"
          className="absolute top-3 right-3 z-10 inline-flex h-9 w-9 items-center justify-center rounded-full bg-black/55 text-white hover:bg-black/75 focus-visible:ring-2 focus-visible:ring-white focus-visible:outline-none"
        >
          <X aria-hidden="true" className="h-4 w-4" />
        </DialogClose>
      </DialogContent>
    </Dialog>
  );
}
