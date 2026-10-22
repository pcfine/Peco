import { useRef, useState } from "react";
import { FileText } from "lucide-react";
import { formatMessageTime } from "@/lib/time";
import type { ChatMessage } from "./ChatView";
import { MESSAGE_TIME_CLASS } from "./messageTime";
import { UserImageLightbox } from "./UserImageLightbox";

/** 三档上限，键由该条消息的图片张数决定。边框分两处：1/2 张画在 img 上（尺寸跟着图走），
 *  3+ 画在单元上（单元定尺，图片 letterbox 在框内不放大）。 */
type Size = "single" | "pair" | "many";

const TRIGGER_CLASS: Record<Size, string> = {
  // 百分比上限必须挂在按钮上：flex 收缩先压按钮，img 的 max-w-full 才解析到已收缩宽度
  single: "min-w-0 max-w-[min(320px,100%)] cursor-zoom-in rounded-md p-0",
  pair: "min-w-0 max-w-[calc(50%-0.25rem)] cursor-zoom-in rounded-md p-0",
  // 每行 3 张时 3×102+2×8=322 会超过窄列的 80% 宽，靠 min-w-0 + shrink 让单元跟着收缩，
  // aspect-square 让高度跟随宽度 —— 高度写百分比对 auto 高度的容器无解
  many: "flex w-[102px] min-w-0 shrink cursor-zoom-in aspect-square items-center justify-center overflow-hidden rounded-md border border-border bg-transparent p-0",
};

/** 三档共用：浏览器默认 outline 是近黑色，深色主题下与页面底色几乎同色，键盘用户看不到焦点
 *  落在哪张图上。焦点环改用 --ring，明暗两套都是中蓝。ring 是 box-shadow、画在盒子外，
 *  many 档的 overflow-hidden 裁不到。 */
const TRIGGER_FOCUS_CLASS =
  "outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/35";

const IMAGE_CLASS: Record<Size, string> = {
  single:
    "block h-auto w-auto max-h-[320px] max-w-full rounded-md border border-border object-contain",
  pair: "block h-auto w-auto max-h-[156px] max-w-full rounded-md border border-border object-contain",
  many: "block max-h-full max-w-full object-contain",
};

const ERROR_CLASS: Record<Size, string> = {
  // 父级行宽由内容撑开，百分比宽度在那上面解析会退化：先给确定宽度，再由 `max-w-full` 保证窄列收缩不溢出
  single:
    "flex h-[120px] w-[320px] max-w-full items-center justify-center rounded-md border border-border bg-muted text-xs text-muted-foreground",
  pair: "flex h-[120px] w-[min(156px,100%)] items-center justify-center rounded-md border border-border bg-muted text-xs text-muted-foreground",
  many: "flex h-[120px] w-[102px] min-w-0 shrink items-center justify-center rounded-md border border-border bg-muted text-xs text-muted-foreground",
};

const ROW_CLASS = "flex justify-end gap-2 max-w-full";
/** 2 张图的行必须定宽：按钮上的百分比上限（`calc(50%-0.25rem)`）挂在宽度由内容撑开的行上会
 *  退化，img 的 `max-w-full` 再取其百分比，400×400 的图实测被压成 129 而不是 156。
 *  定宽 320 = 2×156 + gap 8，`max-w-full` 保证窄列仍按比例收缩不溢出。 */
const PAIR_ROW_CLASS = `${ROW_CLASS} w-[320px]`;

const TEXT_BLOCK_CLASS =
  "bg-primary text-primary-foreground rounded-2xl rounded-br-md px-4 py-2 text-sm max-w-full";

function sizeOf(total: number): Size {
  if (total === 1) return "single";
  if (total === 2) return "pair";
  return "many";
}

/** 单图视图名与触发按钮名刻意不同：一个是「视图」，一个是「控件」 */
function lightboxLabel(index: number, total: number): string {
  return total === 1 ? "图片预览" : `第 ${index + 1} 张图片，共 ${total} 张`;
}

function triggerLabel(index: number, total: number): string {
  return total === 1
    ? "放大查看图片"
    : `放大查看第 ${index + 1} 张图片，共 ${total} 张`;
}

function chunk<T>(items: T[], size: number): T[][] {
  const rows: T[][] = [];
  for (let i = 0; i < items.length; i += size) {
    rows.push(items.slice(i, i + size));
  }
  return rows;
}

export function UserBubble({ message }: { message: ChatMessage }) {
  const time = formatMessageTime(message.timestampMs);
  const [previewIndex, setPreviewIndex] = useState<number | null>(null);
  const triggerElRef = useRef<HTMLButtonElement | null>(null);
  const images = message.images ?? [];
  const attachments = message.attachments ?? [];
  const timeNode = time ? (
    <time
      dateTime={time.iso}
      data-slot="message-time"
      className={`${MESSAGE_TIME_CLASS} right-0`}
    >
      {time.label}
    </time>
  ) : null;

  if (images.length === 0) {
    // 无图（含只有文档附件的消息）沿用单气泡结构：文字与 chip 都在蓝底里
    return (
      <div className="flex justify-end">
        <div
          data-slot="user-bubble"
          className="group relative bg-primary text-primary-foreground rounded-2xl rounded-br-md px-4 py-2 max-w-[80%] text-sm"
        >
          <AttachmentChips items={attachments} withMargin />
          {message.content}
          {timeNode}
        </div>
      </div>
    );
  }

  const total = images.length;
  const size = sizeOf(total);
  const rowClass = size === "pair" ? PAIR_ROW_CLASS : ROW_CLASS;
  const rows = chunk(
    images.map((src, index) => ({ src, index })),
    3,
  );
  // 空白正文不是内容：`" "` / `"\u00a0"` 不该造出一个空蓝药丸；附件 chip 要占一个气泡
  const showTextBlock = message.content.trim() !== "" || attachments.length > 0;
  // 消息被重渲染后下标可能越界，越界即视为未打开
  const opened =
    previewIndex !== null && images[previewIndex] !== undefined
      ? previewIndex
      : null;

  return (
    <div className="flex justify-end">
      <div
        data-slot="user-bubble"
        className="group relative flex flex-col items-end gap-1.5 max-w-[80%]"
      >
        <div
          data-slot="user-images"
          className="flex flex-col items-end gap-1.5 max-w-full"
        >
          {rows.map((row) => (
            <div
              key={row[0].index}
              data-slot="user-image-row"
              className={rowClass}
            >
              {row.map(({ src, index }) => (
                <UserImageBlock
                  key={index}
                  src={src}
                  index={index}
                  total={total}
                  onOpen={(target, trigger) => {
                    triggerElRef.current = trigger;
                    setPreviewIndex(target);
                  }}
                />
              ))}
            </div>
          ))}
        </div>
        {showTextBlock && (
          <div data-slot="user-text" className={TEXT_BLOCK_CLASS}>
            <AttachmentChips
              items={attachments}
              withMargin={message.content.trim() !== ""}
            />
            {message.content}
          </div>
        )}
        {timeNode}
      </div>
      <UserImageLightbox
        image={
          opened === null
            ? null
            : { src: images[opened], label: lightboxLabel(opened, total) }
        }
        triggerElRef={triggerElRef}
        onClose={() => setPreviewIndex(null)}
      />
    </div>
  );
}

interface UserImageBlockProps {
  src: string;
  index: number;
  total: number;
  onOpen: (index: number, trigger: HTMLButtonElement) => void;
}

function UserImageBlock({ src, index, total, onOpen }: UserImageBlockProps) {
  const size = sizeOf(total);
  // 空 src 在浏览器里也不保证触发 error，直接判失败；jsdom 不加载资源，其余失败态靠 onError
  const [failed, setFailed] = useState(src.trim() === "");

  if (failed) {
    return (
      <div
        data-slot="user-image-error"
        data-size={size}
        className={ERROR_CLASS[size]}
      >
        图片加载失败
      </div>
    );
  }

  return (
    <button
      type="button"
      data-slot="user-image-trigger"
      aria-label={triggerLabel(index, total)}
      aria-haspopup="dialog"
      className={`${TRIGGER_CLASS[size]} ${TRIGGER_FOCUS_CLASS}`}
      onClick={(event) => onOpen(index, event.currentTarget)}
    >
      <img
        src={src}
        alt="用户图片"
        className={IMAGE_CLASS[size]}
        onError={() => setFailed(true)}
      />
    </button>
  );
}

function AttachmentChips({
  items,
  withMargin = false,
}: {
  items: { name: string }[];
  withMargin?: boolean;
}) {
  if (items.length === 0) return null;

  return (
    <div
      className={`${withMargin ? "mb-2 " : ""}flex flex-wrap justify-end gap-2`}
    >
      {items.map((att, i) => (
        <span
          key={`${att.name}-${i}`}
          className="inline-flex max-w-[220px] items-center gap-1 rounded-md border border-primary-foreground/30 bg-primary-foreground/10 px-2 py-1 text-xs"
        >
          <FileText className="h-3.5 w-3.5 shrink-0" />
          <span className="truncate">{att.name}</span>
        </span>
      ))}
    </div>
  );
}
