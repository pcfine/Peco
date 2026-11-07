// 唤醒消息的折叠行 —— 它是一条 user 消息，但不是用户说的话：展开前只占一行，
// 正文原样留在展开体里，用户点开的与模型收到的始终是同一段文本。

import { useState } from "react";
import { Bell, ChevronRight } from "lucide-react";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { CHEVRON, FOLD_ROW } from "@/components/chat/foldRow";
import { MESSAGE_TIME_CLASS } from "@/components/chat/messageTime";
import { formatMessageTime } from "@/lib/time";

/** 投递文本的首行前缀：整条消息靠它认出来，别处不重复这个字面量。 */
const WAKE_PREFIX = "[peco-wake]";

/** 前缀之后的首行形状，项数只从这里取；行尾多一个空格不算另一种格式。 */
const HEADER_PATTERN = /^后台任务结果（(\d+)\s*项）$/;

const LABEL = "后台任务通知";

export function isWakeNotice(content: string): boolean {
  return content.trimStart().startsWith(WAKE_PREFIX);
}

/**
 * 首行记的项数，认不出格式就返回 null。
 *
 * 拿不到时只省掉项数：正文照旧可展开，猜一个数字比不显示更糟 —— 项数是
 * 「这一批处理了几条」的事实，不是排版细节。
 */
function itemCountOf(content: string): number | null {
  const firstLine = content.split("\n", 1)[0].trim();
  if (!firstLine.startsWith(WAKE_PREFIX)) return null;
  const header = firstLine.slice(WAKE_PREFIX.length).trim();
  const matched = HEADER_PATTERN.exec(header);
  if (!matched) return null;
  const count = Number(matched[1]);
  return Number.isSafeInteger(count) ? count : null;
}

export interface WakeNoticeProps {
  content: string;
  timestampMs?: number;
}

export function WakeNotice({ content, timestampMs }: WakeNoticeProps) {
  const [open, setOpen] = useState(false);
  const itemCount = itemCountOf(content);
  const label = itemCount === null ? LABEL : `${LABEL} · ${itemCount} 项`;
  const time = formatMessageTime(timestampMs);

  return (
    // `group relative` 只为时间戳：它沿用气泡那套 hover 才出现、不占布局的定位。
    <div data-slot="wake-notice" className="group relative min-w-0">
      <Collapsible open={open} onOpenChange={setOpen}>
        <CollapsibleTrigger data-fold-row="wake" className={FOLD_ROW}>
          <ChevronRight
            data-slot="wake-chevron"
            className={CHEVRON}
            aria-hidden="true"
          />
          <span
            data-fold-content
            className="flex min-w-0 flex-1 items-center gap-2"
          >
            <Bell
              className="h-3.5 w-3.5 shrink-0 text-muted-foreground"
              aria-hidden="true"
            />
            <span
              data-slot="wake-label"
              className="truncate text-muted-foreground"
            >
              {label}
            </span>
          </span>
        </CollapsibleTrigger>
        <CollapsibleContent className="mt-1 rounded-md border border-border bg-tool-surface px-3 py-2">
          <div
            data-slot="wake-body"
            className="whitespace-pre-wrap break-words text-xs text-muted-foreground"
          >
            {content}
          </div>
        </CollapsibleContent>
      </Collapsible>
      {time && (
        <time
          dateTime={time.iso}
          data-slot="message-time"
          className={`${MESSAGE_TIME_CLASS} right-0`}
        >
          {time.label}
        </time>
      )}
    </div>
  );
}
