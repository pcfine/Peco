// 气泡下方那行元信息：时间戳 + 复制按钮。两者共用一条 hover 才显形、不占布局的
// 绝对定位行 —— 分开定位会让「时间贴边、按钮挨着时间」在窄气泡里各自为政。

import { useEffect, useRef, useState } from "react";
import { Check, Copy } from "lucide-react";
import { toast } from "sonner";
import { copyText } from "@/lib/clipboard";

/** 复制成功后勾标停留时长：够看清，又不至于让用户以为状态卡住了。 */
const COPIED_FEEDBACK_MS = 1500;

/**
 * 零间隙（`top-full` 且无外边距）是悬停链的成立条件：行容器是气泡 `group` 的子
 * 节点，指针落在按钮上时祖先 `:hover` 仍成立，盒边相接即可一路悬停过来。
 * 可点垫片只给按钮（`group-hover:pointer-events-auto` 在按钮上），行容器不给 ——
 * `align="start"` 的行容器是 `left-0 right-0` 全宽，一旦可点就成一条 24px 高的
 * 透明带，挡住下一条消息的点击。
 * 行高 24px 与按钮同高，时间戳与图标按同一条中线居中。
 */
const ROW_CLASS =
  "pointer-events-none select-none absolute top-full z-10 flex h-6 items-center gap-2 whitespace-nowrap opacity-0 group-hover:opacity-100 focus-within:opacity-100";

/**
 * 子节点顺序恒为 [时间, 按钮]，且行方向不反转：按钮靠气泡的 `group-hover` 显形，
 * 指针从气泡下沿落到按钮之间不能有断点，所以按钮必须贴在气泡的左/右边界内侧、
 * 与气泡的 x 区间重叠。用户气泡若反转行方向把时间顶到最右，窄气泡（58px 的「你好」
 * 撑不下 60px 时间戳 + 24px 按钮）会让整行向左溢出，按钮落到气泡左边界之外，
 * 悬停链在气泡与按钮之间断开，按钮再也拿不回来。
 * 代价：用户气泡的时间戳距右边界 32px（按钮 24px + 间距 8px）。
 */
const ALIGN_CLASS = {
  start: "left-0 right-0 justify-between",
  end: "right-0",
} as const;

const TIME_CLASS = "text-2xs leading-none text-muted-foreground";

/**
 * 行容器 pointer-events-none、按钮默认也 none：否则每个气泡下方都埋一个看不见
 * 却可点的热区，鼠标扫过气泡边缘就会误触。
 */
const COPY_BUTTON_CLASS =
  "pointer-events-none grid size-6 shrink-0 place-items-center rounded-sm text-muted-foreground transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring outline-none group-hover:pointer-events-auto focus-visible:pointer-events-auto";

const COPY_ICON_CLASS = "size-3.5";

const COPY_LABEL = "复制消息文本";
const COPIED_LABEL = "已复制";

export interface MessageMetaProps {
  time: { label: string; iso: string } | null;
  /** 省略或只有空白 = 这条消息没有可复制正文，不渲染按钮。 */
  copyText?: string;
  /** `start`：左对齐气泡（助手）；`end`：右对齐气泡（用户）。 */
  align: "start" | "end";
}

export function MessageMeta({ time, copyText: text, align }: MessageMetaProps) {
  const [copied, setCopied] = useState(false);
  const timerRef = useRef<number | null>(null);

  useEffect(() => {
    return () => {
      if (timerRef.current !== null) window.clearTimeout(timerRef.current);
    };
  }, []);

  // 纯空白复制出去等于什么都没拿到，不该白留一个可点热区
  const copySource =
    text !== undefined && text.trim() !== "" ? text : undefined;

  if (!time && copySource === undefined) return null;

  const handleCopy = async () => {
    if (copySource === undefined) return;
    if (!(await copyText(copySource))) {
      // 失败不改图标：留着可复制的样子，用户重试或手选都还有路。
      toast.error("复制失败，请手动选择文本");
      return;
    }
    setCopied(true);
    if (timerRef.current !== null) window.clearTimeout(timerRef.current);
    timerRef.current = window.setTimeout(() => {
      timerRef.current = null;
      setCopied(false);
    }, COPIED_FEEDBACK_MS);
  };

  const label = copied ? COPIED_LABEL : COPY_LABEL;

  return (
    <div
      data-slot="message-meta"
      className={`${ROW_CLASS} ${ALIGN_CLASS[align]}`}
    >
      {time && (
        <time
          dateTime={time.iso}
          data-slot="message-time"
          className={TIME_CLASS}
        >
          {time.label}
        </time>
      )}
      {copySource !== undefined && (
        <button
          type="button"
          data-slot="message-copy"
          aria-label={label}
          title={label}
          className={COPY_BUTTON_CLASS}
          onClick={handleCopy}
        >
          {copied ? (
            <Check className={COPY_ICON_CLASS} aria-hidden="true" />
          ) : (
            <Copy className={COPY_ICON_CLASS} aria-hidden="true" />
          )}
        </button>
      )}
    </div>
  );
}
