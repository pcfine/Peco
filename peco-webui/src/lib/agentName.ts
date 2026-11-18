// Agent 的中文展示名从 description 推导，不单独存字段：save_agent 创建的 agent
// 只写 agent.md，给它补一列显示名会让两种创建路径分叉。
//
// 约定：description 写成「名称 — 说明」，标题取分隔符前的名称。只在开头这么短的
// 窗口里找分隔符：名称写多长由各人习惯决定，「说明里随手一个连字符」不该把标题
// 换成前半句废话，超出窗口就整条回退到 agent id。

const SEPARATORS = ["-", "—", "–"];
const TITLE_CHARS = 12;

/** 最早出现的分隔符下标，没有则 -1。数据里 ASCII 连字符与全角破折号混用。 */
function firstSeparatorIndex(text: string): number {
  let cut = -1;
  for (const separator of SEPARATORS) {
    const index = text.indexOf(separator);
    if (index >= 0 && (cut < 0 || index < cut)) cut = index;
  }
  return cut;
}

/** 卡片/编辑器标题用的展示名；推导不出时回退到 agent id。 */
export function agentDisplayName(
  description: string,
  fallback: string,
): string {
  const text = description.trim();
  if (!text) return fallback;

  // 按码点开窗：slice 切 UTF-16 会留下半个 emoji。
  const window = Array.from(text).slice(0, TITLE_CHARS).join("");
  const cut = firstSeparatorIndex(window);
  // 分隔符落在开头说明前面没名称，这条只能回退到 id。
  return cut > 0 ? window.slice(0, cut).trim() : fallback;
}

/**
 * 整条 description 里的角色名。标题窗口装不下名称时，卡片副行靠它把名称留住：
 * 标题此时是 id，副行再写一遍 id 就等于什么都没说。
 */
export function agentRoleName(description: string): string {
  const text = description.trim();
  const cut = firstSeparatorIndex(text);
  return cut > 0 ? text.slice(0, cut).trim() : "";
}

/** 卡片副文本：去掉「名称 —」前缀后的说明；无分隔符时原样返回。 */
export function agentDescriptionDetail(description: string): string {
  const text = description.trim();
  if (!text) return "";

  const cut = firstSeparatorIndex(text);
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
