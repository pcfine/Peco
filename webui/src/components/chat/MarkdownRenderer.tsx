// MarkdownRenderer — renders AI-generated markdown with GFM + syntax highlighting

import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";

interface MarkdownRendererProps {
  content: string;
}

export function MarkdownRenderer({ content }: MarkdownRendererProps) {
  return (
    <div
      className="max-w-none break-words text-sm leading-relaxed space-y-2
      [&_h1]:text-xl [&_h1]:font-bold [&_h1]:mt-4 [&_h1]:mb-2
      [&_h2]:text-lg [&_h2]:font-semibold [&_h2]:mt-3 [&_h2]:mb-2
      [&_h3]:text-base [&_h3]:font-semibold [&_h3]:mt-3 [&_h3]:mb-1
      [&_p]:my-1
      [&_ul]:list-disc [&_ul]:pl-5 [&_ul]:my-1
      [&_ol]:list-decimal [&_ol]:pl-5 [&_ol]:my-1
      [&_li]:my-0.5
      [&_strong]:font-semibold
      [&_em]:italic
      [&_hr]:my-4 [&_hr]:border-border
      [&_pre]:bg-muted/70 [&_pre]:rounded-md [&_pre]:p-3 [&_pre]:overflow-x-auto [&_pre]:my-2
      [&_code]:text-xs [&_code]:bg-muted/70 [&_code]:rounded [&_code]:px-1 [&_code]:py-0.5
      [&_pre_code]:bg-transparent [&_pre_code]:p-0 [&_pre_code]:rounded-none
      [&_table]:border-collapse [&_table]:w-full [&_table]:my-2
      [&_th]:border [&_th]:border-border [&_th]:px-3 [&_th]:py-1.5 [&_th]:bg-muted/50 [&_th]:text-left [&_th]:font-semibold
      [&_td]:border [&_td]:border-border [&_td]:px-3 [&_td]:py-1.5
      [&_blockquote]:border-l-4 [&_blockquote]:border-l-muted-foreground/30 [&_blockquote]:pl-3 [&_blockquote]:text-muted-foreground [&_blockquote]:my-2
      [&_a]:text-primary [&_a]:underline [&_a]:break-all
      [&_img]:rounded-md [&_img]:max-w-full [&_img]:my-2
    "
    >
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[rehypeHighlight]}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}
