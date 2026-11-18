/** 切开后的两半；两段拼回去必须逐字节等于原文。 */
export interface FrontmatterSplit {
  /** 含首尾两条 `---` 行，但不含闭合行后的换行；没有 frontmatter 时为 null。 */
  frontmatter: string | null;
  /** 闭合行之后的剩余内容，保留其开头的换行。 */
  body: string;
}

const BOM = "\uFEFF";
/** 首行必须是独占一行的 `---`，故此处的换行不可省。 */
const OPEN_FENCE = /^---[ \t]*\r?\n/;
/** `m` 让 `^` 落在每行开头；`\r` 属于上一行的行尾，不会被误算进本行。 */
const CLOSE_FENCE = /^---[ \t]*(?=\r?\n|$)/m;

/**
 * 按 remark-frontmatter 的判定切出 frontmatter：只有首行是 `---` 且后续存在
 * 独占一行的闭合 `---` 才算，正文中间的 `---` 一律不处理。
 *
 * 只做字符串切分、不做 YAML 解析 —— 调用方要么原样展示，要么按别的语言高亮。
 */
export function splitFrontmatter(content: string): FrontmatterSplit {
  const start = content.startsWith(BOM) ? BOM.length : 0;
  const open = OPEN_FENCE.exec(content.slice(start));
  if (!open) return { frontmatter: null, body: content };

  const afterOpen = start + open[0].length;
  const close = CLOSE_FENCE.exec(content.slice(afterOpen));
  if (!close) return { frontmatter: null, body: content };

  // 切点落在闭合行的行尾之后、换行之前，body 才能自己带着换行继续。
  const end = afterOpen + close.index + close[0].length;
  return { frontmatter: content.slice(0, end), body: content.slice(end) };
}

export function hasFrontmatter(content: string): boolean {
  return splitFrontmatter(content).frontmatter !== null;
}
