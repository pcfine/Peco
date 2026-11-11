// Agent 的中文展示名从 description 推导，不单独存字段：save_agent 创建的 agent
// 只写 agent.md，给它补一列显示名会让两种创建路径分叉。
//
// 约定：description 的「名称 — 说明」形式取破折号前的名称；没有分隔符时取前
// 3 个字符；结果最多 8 个字符，避免卡片标题被一整句话撑开。

const SEPARATORS = ["-", "—", "–"];
const MAX_NAME_CHARS = 8;
const HEAD_CHARS = 3;

function takeChars(text: string, limit: number): string {
  const chars = Array.from(text);
  if (chars.length <= limit) return text.trimEnd();

  let cut = chars.slice(0, limit).join("");
  // 截断点落在单词中间时回退到前一个空白，避免「记忆管理 Age」这种半截词。
  const next = chars[limit];
  if (next && !/\s/.test(next) && !/\s/.test(cut.at(-1) ?? "")) {
    const boundary = cut.search(/\s\S*$/);
    if (boundary > 0) cut = cut.slice(0, boundary);
  }
  return cut.trimEnd();
}

/** 卡片/编辑器标题用的展示名；推导不出时回退到 agent id。 */
export function agentDisplayName(
  description: string,
  fallback: string,
): string {
  const text = description.trim();
  if (!text) return fallback;

  let cut = -1;
  for (const separator of SEPARATORS) {
    const index = text.indexOf(separator);
    if (index >= 0 && (cut < 0 || index < cut)) cut = index;
  }

  if (cut > 0) {
    const name = takeChars(text.slice(0, cut), MAX_NAME_CHARS);
    if (name) return name;
  }

  // 无分隔符，或分隔符前为空：去掉开头的分隔符/空白后取前几个字。
  const head = text.replace(/^[-—–\s]+/, "");
  return takeChars(head, HEAD_CHARS) || fallback;
}

/** 卡片副文本：去掉「名称 —」前缀后的说明；无分隔符时原样返回。 */
export function agentDescriptionDetail(description: string): string {
  const text = description.trim();
  if (!text) return "";

  let cut = -1;
  for (const separator of SEPARATORS) {
    const index = text.indexOf(separator);
    if (index >= 0 && (cut < 0 || index < cut)) cut = index;
  }
  return cut >= 0 ? text.slice(cut + 1).trim() : text;
}

/** 首个字素：中文取首字，组合 emoji 也不会被截成半个。 */
export function firstGrapheme(text: string): string {
  const value = text.trim();
  if (!value) return "";
  if (typeof Intl !== "undefined" && "Segmenter" in Intl) {
    const first = new Intl.Segmenter("zh", { granularity: "grapheme" })
      .segment(value)
      [Symbol.iterator]()
      .next();
    if (!first.done) return first.value.segment;
  }
  return Array.from(value)[0] ?? "";
}
