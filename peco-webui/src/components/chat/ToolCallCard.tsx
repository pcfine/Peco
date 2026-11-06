// 工具调用卡片 —— 折叠行（工具名 + 执行参数 + 状态）与展开体（输出在前、参数垫底）。
//
// 收起行给参数、展开体先给输出：收起态读的是「跑了什么命令」，展开时的问题变成
// 「跑出了什么」，两者信息互补而非重复。折叠行静态透明、只给 hover 反馈 —— 卡片
// 与正文同色会让二者混成一片，分层靠「透明行 + 描边体」而不是第二种底色。

import { memo, useState } from "react";
import { Check, ChevronRight, CircleDashed, Loader2 } from "lucide-react";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { CHEVRON, FOLD_ROW } from "@/components/chat/foldRow";
import {
  ARGUMENTS_CHAR_CAP,
  OUTPUT_CHAR_CAP,
  SUMMARY_CHAR_CAP,
  capText,
  flattenWhitespace,
  summarizeArguments,
  type ToolCallStatus,
  type ToolCallView,
} from "@/lib/toolCall";

const STATUS_TEXT: Record<ToolCallStatus, string> = {
  pending: "执行中",
  done: "已完成",
  missing: "输出缺失",
};

/** aria-label 里的轨文本上限：读屏器不该逐字念完整段工具输出。 */
const TRACK_LABEL_CHAR_CAP = 80;

function StatusIcon({ status }: { status: ToolCallStatus }) {
  if (status === "pending") {
    return (
      <Loader2
        className="h-3.5 w-3.5 animate-spin text-muted-foreground"
        aria-hidden="true"
      />
    );
  }
  if (status === "missing") {
    return (
      <CircleDashed className="h-3.5 w-3.5 text-warning" aria-hidden="true" />
    );
  }
  return (
    <Check className="h-3.5 w-3.5 text-muted-foreground" aria-hidden="true" />
  );
}

interface OutputFlags {
  result: string;
  hasOutput: boolean;
  imageCount: number;
}

/**
 * 折叠行、展开体、空态三处共用同一组判据 —— 各算各的会出现「行说空、体说非空」。
 * 判据只看 `result` 是否存在，不参与状态判断：`status` 是唯一权威。
 */
function outputFlagsOf(toolCall: ToolCallView): OutputFlags {
  const result = toolCall.result ?? "";
  return {
    result,
    hasOutput: result.trim() !== "",
    imageCount: toolCall.images?.length ?? 0,
  };
}

type TrackTone = "args" | "muted";

interface Track {
  text: string;
  tone: TrackTone;
  /** 占位文案不是真实内容，不进 aria-label。 */
  placeholder: boolean;
}

/** 两档都留在过程层：参数是过程的信息主体，占位文案只是过程标签。 */
const TRACK_TONE_CLASS: Record<TrackTone, string> = {
  args: "text-tool-args-fg",
  muted: "text-muted-foreground",
};

/** 收起行第二轨显示执行参数 —— 这一行被读成「跑了什么命令」，不是「跑出什么」。 */
function trackOf(toolCall: ToolCallView): Track {
  // 空壳先挡在摘要之前：`summarizeArguments` 对非对象 JSON 退回原文，不挡的话
  // `[]` / `null` 会被当成摘要送上轨。
  const blank = isArgsBlank(toolCall.arguments);
  const summary = blank ? "" : summarizeArguments(toolCall.arguments);
  if (summary !== "") {
    return { text: summary, tone: "args", placeholder: false };
  }

  // 摘要未命中候选键不等于没有参数：`{"steps":[…]}` 这类只露原文才不会对
  // 「有参数」做出事实性错误。
  if (!blank) {
    const raw = capText(
      flattenWhitespace(toolCall.arguments),
      SUMMARY_CHAR_CAP,
    ).text;
    if (raw !== "") {
      return { text: raw, tone: "args", placeholder: false };
    }
  }

  // 只返回图片的工具：图片是唯一产出，轨上必须说出来，否则与空输出无法区分。
  const imageCount = toolCall.images?.length ?? 0;
  if (imageCount > 0) {
    return {
      text: `输出 ${imageCount} 张图片`,
      tone: "args",
      placeholder: false,
    };
  }

  // 与展开体的「参数未记录」同口径：前端分不清「本就无参数」与「参数没被记下来」。
  return { text: "（参数未记录）", tone: "muted", placeholder: true };
}

/** 参数是否为空壳。空壳没有信息量，显示 `{}` / `[]` 只是噪音；解析后判空比比对字面量可靠。 */
function isArgsBlank(argsJson: string): boolean {
  const trimmed = argsJson.trim();
  if (trimmed === "") return true;
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    // 非 JSON 原文仍有信息量
    return false;
  }
  if (parsed === null) return true;
  if (Array.isArray(parsed)) return parsed.length === 0;
  if (typeof parsed === "object") return Object.keys(parsed).length === 0;
  // "abc" / 42 / true 仍有信息量
  return false;
}

interface EmptyState {
  reason: "pending" | "missing" | "empty";
  text: string;
}

/**
 * 输出区的空态。`done` 且无输出也走这里：`status` 是唯一权威，`result`
 * 缺失不等于「什么都不画」——静默的空体会让人以为输出丢了。
 *
 * 只返回图片的工具（有图片、无文本）不是空输出 —— 占位会把唯一的产出
 * 挡在展开体之外。
 */
function emptyStateOf(
  toolCall: ToolCallView,
  flags: OutputFlags,
): EmptyState | null {
  if (toolCall.status === "pending") {
    return { reason: "pending", text: "等待工具返回…" };
  }
  if (toolCall.status === "missing") {
    return { reason: "missing", text: "本轮未记录到该工具的输出" };
  }
  if (!flags.hasOutput && flags.imageCount === 0) {
    return { reason: "empty", text: "（工具返回空输出）" };
  }
  return null;
}

function TruncatedHint({ total, cap }: { total: number; cap: number }) {
  return (
    <p
      data-slot="tool-truncated"
      className="mt-1 text-xs text-muted-foreground"
    >
      已截断：仅显示前 {cap} 字符（共 {total} 字符）
    </p>
  );
}

export interface ToolCallCardProps {
  toolCall: ToolCallView;
}

export const ToolCallCard = memo(function ToolCallCard({
  toolCall,
}: ToolCallCardProps) {
  const [open, setOpen] = useState(false);

  const flags = outputFlagsOf(toolCall);
  const images = toolCall.images ?? [];
  const args = capText(toolCall.arguments, ARGUMENTS_CHAR_CAP);
  const output = capText(flags.result, OUTPUT_CHAR_CAP);
  const track = trackOf(toolCall);
  const empty = emptyStateOf(toolCall, flags);
  const label = track.placeholder
    ? `${toolCall.name}，${STATUS_TEXT[toolCall.status]}`
    : `${toolCall.name}，${STATUS_TEXT[toolCall.status]}，${
        capText(track.text, TRACK_LABEL_CHAR_CAP).text
      }`;

  return (
    <li
      data-slot="tool-call-card"
      data-tool-id={toolCall.id}
      data-tool-name={toolCall.name}
      data-status={toolCall.status}
      data-orphan={toolCall.orphan ? "true" : undefined}
      className="min-w-0"
    >
      <Collapsible open={open} onOpenChange={setOpen}>
        <CollapsibleTrigger
          data-fold-row="tool"
          className={FOLD_ROW}
          aria-label={label}
        >
          <ChevronRight
            data-slot="tool-chevron"
            className={CHEVRON}
            aria-hidden="true"
          />
          <span
            data-fold-content
            className="flex min-w-0 flex-1 items-center gap-2"
          >
            <span
              data-slot="tool-name"
              className="shrink-0 truncate font-mono text-muted-foreground max-w-[45%]"
            >
              {toolCall.name}
            </span>
            <span
              data-slot="tool-line"
              className={`min-w-0 flex-1 truncate font-mono ${TRACK_TONE_CLASS[track.tone]}`}
            >
              {track.text}
            </span>
            <span data-slot="tool-status" className="ml-auto shrink-0">
              <StatusIcon status={toolCall.status} />
            </span>
          </span>
        </CollapsibleTrigger>

        <CollapsibleContent className="mt-1 rounded-md border border-border bg-tool-surface px-3 py-2">
          <div className="flex flex-col gap-2">
            {toolCall.status === "done" && flags.hasOutput && (
              <div data-block="output" className="min-w-0">
                <pre
                  data-slot="tool-pre"
                  data-section="output"
                  className="font-mono text-xs text-foreground min-w-0 max-h-[320px] overflow-auto overscroll-contain whitespace-pre"
                >
                  {output.text}
                </pre>
                {output.truncated && (
                  <TruncatedHint total={output.total} cap={OUTPUT_CHAR_CAP} />
                )}
              </div>
            )}

            {/* 图片与文本输出同权：status 不是 done 时二者都不渲染 */}
            {toolCall.status === "done" && images.length > 0 && (
              <div className="flex flex-wrap gap-2">
                {images.map((url, i) => (
                  <img
                    key={i}
                    src={url}
                    alt={`${toolCall.name} 输出图片`}
                    className="max-h-48 max-w-[240px] rounded-md border object-contain"
                  />
                ))}
              </div>
            )}

            {empty && (
              <p
                data-slot="tool-empty"
                data-reason={empty.reason}
                className="text-xs text-muted-foreground"
              >
                {empty.text}
              </p>
            )}

            {args.text !== "" ? (
              <div className="border-t border-border pt-2">
                <pre
                  data-slot="tool-pre"
                  data-section="arguments"
                  className="font-mono text-xs text-tool-args-fg whitespace-pre-wrap break-words max-h-[88px] overflow-y-auto overscroll-contain"
                >
                  {args.text}
                </pre>
                {args.truncated && (
                  <TruncatedHint total={args.total} cap={ARGUMENTS_CHAR_CAP} />
                )}
              </div>
            ) : (
              <p
                data-slot="tool-args-missing"
                className="text-xs text-muted-foreground"
              >
                参数未记录
              </p>
            )}
          </div>
        </CollapsibleContent>
      </Collapsible>
    </li>
  );
});
