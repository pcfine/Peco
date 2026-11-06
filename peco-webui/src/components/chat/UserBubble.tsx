import { useRef, useState } from "react";
import { FileText } from "lucide-react";
import { formatMessageTime } from "@/lib/time";
import type { ChatMessage } from "./ChatView";
import { MESSAGE_TIME_CLASS } from "./messageTime";
import { UserImageLightbox } from "./UserImageLightbox";

/** 96px 方形缩略图，`w-24` 是上限；窄列下靠 min-w-0 + shrink 收缩 */
const IMAGE_TRIGGER_CLASS =
  "flex w-24 min-w-0 shrink cursor-zoom-in aspect-square items-center justify-center overflow-hidden rounded-md border border-border bg-transparent p-0";

/** cover 裁切铺满按钮，圆角与边框由按钮承担 */
const IMAGE_CLASS = "block h-full w-full object-cover";

/** 默认 outline 在深色主题下几乎不可见，改用 ring token */
const TRIGGER_FOCUS_CLASS =
  "outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/35";

const ERROR_CLASS =
  "flex w-24 min-w-0 shrink aspect-square items-center justify-center rounded-md border border-border bg-muted text-xs text-muted-foreground";

/** 缩略图行左对齐，与输入框「文字在上、附件在下」一致 */
const ROW_CLASS = "flex justify-start gap-1.5 max-w-full";

/** 每行图片上限；气泡是 shrink-to-fit，宽度取正文与图片行中较宽者 */
const MAX_IMAGES_PER_ROW = 6;

const TEXT_BLOCK_CLASS =
  "bg-primary text-primary-foreground rounded-2xl rounded-br-md px-4 py-2 text-sm max-w-full";

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
  // 空白正文不算内容，不造空气泡
  const hasText = message.content.trim() !== "";

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
    // 无图消息沿用单气泡结构
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
  const rows = chunk(
    images.map((src, index) => ({ src, index })),
    MAX_IMAGES_PER_ROW,
  );
  const showTextBlock = hasText || attachments.length > 0;
  // 重渲染后下标可能越界，越界视为未打开
  const opened =
    previewIndex !== null && images[previewIndex] !== undefined
      ? previewIndex
      : null;

  const imageBlock = (
    <div
      data-slot="user-images"
      className={`${hasText ? "mt-2 " : ""}flex flex-col items-start gap-1.5 max-w-full`}
    >
      {rows.map((row) => (
        <div
          key={row[0].index}
          data-slot="user-image-row"
          className={ROW_CLASS}
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
  );

  return (
    <div className="flex justify-end">
      <div
        data-slot="user-bubble"
        className="group relative flex flex-col items-end gap-1.5 max-w-[80%]"
      >
        {showTextBlock ? (
          // 图文同泡：正文在上、缩略图在下
          <div data-slot="user-text" className={TEXT_BLOCK_CLASS}>
            {message.content}
            {imageBlock}
            {attachments.length > 0 && (
              <div className="mt-2">
                <AttachmentChips items={attachments} align="start" />
              </div>
            )}
          </div>
        ) : (
          imageBlock
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
  // 空 src 不保证触发 error，直接判失败；其余失败态靠 onError
  const [failed, setFailed] = useState(src.trim() === "");

  if (failed) {
    return (
      <div data-slot="user-image-error" className={ERROR_CLASS}>
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
      className={`${IMAGE_TRIGGER_CLASS} ${TRIGGER_FOCUS_CLASS}`}
      onClick={(event) => onOpen(index, event.currentTarget)}
    >
      <img
        src={src}
        alt="用户图片"
        className={IMAGE_CLASS}
        onError={() => setFailed(true)}
      />
    </button>
  );
}

function AttachmentChips({
  items,
  withMargin = false,
  align = "end",
}: {
  items: { name: string }[];
  withMargin?: boolean;
  align?: "start" | "end";
}) {
  if (items.length === 0) return null;

  return (
    <div
      className={`${withMargin ? "mb-2 " : ""}flex flex-wrap ${
        align === "start" ? "justify-start" : "justify-end"
      } gap-2`}
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
