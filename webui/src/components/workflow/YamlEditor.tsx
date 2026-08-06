import { useRef, useEffect, useCallback } from "react";
import { cn } from "@/lib/utils";

interface YamlEditorProps {
  value: string;
  onChange: (value: string) => void;
  readOnly?: boolean;
  className?: string;
  placeholder?: string;
}

/**
 * Simple YAML editor with line numbers.
 *
 * Phase 1: Plain textarea with line-number gutter.
 * Phase 2 target: Replace with CodeMirror/Monaco for syntax highlighting.
 */
export function YamlEditor({
  value,
  onChange,
  readOnly = false,
  className,
  placeholder,
}: YamlEditorProps) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const gutterRef = useRef<HTMLDivElement>(null);

  const lineCount = value.split("\n").length;

  const syncScroll = useCallback(() => {
    if (textareaRef.current && gutterRef.current) {
      gutterRef.current.scrollTop = textareaRef.current.scrollTop;
    }
  }, []);

  // Recalculate line numbers on change
  useEffect(() => {
    syncScroll();
  }, [value, syncScroll]);

  return (
    <div
      className={cn(
        "relative flex rounded-md border bg-background font-mono text-sm",
        readOnly && "bg-muted/30",
        className,
      )}
    >
      {/* Line number gutter */}
      <div
        ref={gutterRef}
        className="overflow-hidden shrink-0 select-none border-r bg-muted/50 py-3 pl-3 pr-2 text-right text-xs text-muted-foreground"
        style={{ minWidth: `${Math.max(3, String(lineCount).length + 1)}ch` }}
      >
        {Array.from({ length: Math.max(lineCount, 1) }, (_, i) => (
          <div key={i} className="leading-6 h-6">
            {i + 1}
          </div>
        ))}
      </div>

      {/* Textarea */}
      <textarea
        ref={textareaRef}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onScroll={syncScroll}
        readOnly={readOnly}
        placeholder={placeholder}
        spellCheck={false}
        className={cn(
          "flex-1 resize-none bg-transparent py-3 px-3 leading-6 outline-none",
          "overflow-auto",
          readOnly && "cursor-default",
        )}
        style={{ minHeight: "300px", tabSize: 2 }}
      />
    </div>
  );
}
