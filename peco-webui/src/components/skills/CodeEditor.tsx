import { useLayoutEffect, useMemo, useRef, type RefObject } from "react";
import hljs from "highlight.js/lib/common";
import { splitFrontmatter } from "@/lib/frontmatter";

/** 超过这个长度不做高亮，避免长文件每次击键阻塞主线程。 */
const MAX_HIGHLIGHT_CHARS = 120_000;

const LAYER = [
  "m-0 px-5 py-4 font-mono text-sm leading-5 tracking-normal tab-4",
  "whitespace-pre",
].join(" ");

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

function highlight(code: string, language: string): string {
  if (!code) return "";
  if (code.length > MAX_HIGHLIGHT_CHARS) return escapeHtml(code);
  try {
    return hljs.highlight(code, { language, ignoreIllegals: true }).value;
  } catch {
    return escapeHtml(code);
  }
}

/**
 * Markdown 的 frontmatter 不是 Markdown：highlight.js 会把闭合 `---` 当成
 * setext 下划线，把整段属性吞进一个 section 标题。按 YAML 单独高亮再拼回去，
 * 高亮层是 textarea 的透明叠层，拼接不能增减任何字符。
 *
 * CRLF 要先并成 LF：YAML 高亮会把行尾的 `\r` 收进 meta span，HTML 折行时它
 * 与 span 外的 `\n` 被拆成两个换行，高亮层就比 textarea 多一行、整篇错位。
 */
function highlightMarkdown(code: string): string {
  // 分段后每段都小于阈值，长度闸门必须先按整篇判。
  if (code.length > MAX_HIGHLIGHT_CHARS) return escapeHtml(code);
  const { frontmatter, body } = splitFrontmatter(code);
  if (frontmatter === null) return highlight(code, "markdown");
  return (
    highlight(frontmatter.replaceAll("\r\n", "\n"), "yaml") +
    highlight(body.replaceAll("\r\n", "\n"), "markdown")
  );
}

interface CodeEditorProps {
  value: string;
  onChange?: (next: string) => void;
  language: string;
  readOnly?: boolean;
  ariaLabel: string;
  testId?: string;
  /** 由父组件持有，用于切换文件时恢复滚动位置与选区。 */
  textareaRef: RefObject<HTMLTextAreaElement | null>;
}

/**
 * 高亮编辑器：透明文字的 textarea 叠在高亮层之上，两者共用同一套排版参数。
 * 只有 textarea 有滚动条，高亮层用 transform 跟随，避免出现两条滑条。
 */
export function CodeEditor({
  value,
  onChange,
  language,
  readOnly = false,
  ariaLabel,
  testId,
  textareaRef,
}: CodeEditorProps) {
  const preRef = useRef<HTMLPreElement>(null);

  const html = useMemo(
    () =>
      language === "markdown"
        ? highlightMarkdown(value)
        : highlight(value, language),
    [value, language],
  );

  const syncScroll = () => {
    const textarea = textareaRef.current;
    const pre = preRef.current;
    if (!textarea || !pre) return;
    pre.style.transform = `translate(${-textarea.scrollLeft}px, ${-textarea.scrollTop}px)`;
  };

  // 内容或行数变化会改变 scrollHeight，滚动位置需重新对齐。
  useLayoutEffect(syncScroll, [value]);

  const handleKeyDown = (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key !== "Tab" || readOnly || !onChange) return;
    event.preventDefault();
    const el = event.currentTarget;
    const indent = language === "python" ? "    " : "  ";
    const { selectionStart: start, selectionEnd: end } = el;
    const next = value.slice(0, start) + indent + value.slice(end);
    onChange(next);
    requestAnimationFrame(() => {
      el.selectionStart = start + indent.length;
      el.selectionEnd = start + indent.length;
    });
  };

  return (
    <div
      className="relative h-full min-h-0 w-full overflow-hidden"
      data-code-editor
    >
      <pre
        ref={preRef}
        aria-hidden="true"
        className={`pointer-events-none absolute top-0 left-0 w-max min-w-full ${LAYER}`}
      >
        {/* 末尾补一个换行：<pre> 的行末换行不产生行盒，textarea 会多算一行。 */}
        <code
          className="hljs bg-transparent"
          dangerouslySetInnerHTML={{ __html: `${html}\n` }}
        />
      </pre>
      <textarea
        ref={textareaRef}
        aria-label={ariaLabel}
        data-testid={testId}
        spellCheck={false}
        readOnly={readOnly}
        value={value}
        onChange={(event) => onChange?.(event.target.value)}
        onKeyDown={handleKeyDown}
        onScroll={syncScroll}
        wrap="off"
        className={`absolute inset-0 h-full w-full resize-none overflow-auto rounded-none border-0 bg-transparent text-transparent caret-foreground outline-none [scrollbar-gutter:stable] field-sizing-fixed focus-visible:ring-0 ${LAYER}`}
      />
    </div>
  );
}
