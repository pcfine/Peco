/**
 * 5×5 镜像像素图标（GitHub identicon 风格）。
 *
 * 图案与配色完全由 seed 决定：同一 seed 在表单、列表、对话页渲染出同一张图，
 * 不读 background_color 之类的旁路字段，也不依赖主题。
 */

export const IDENTICON_GRID = 5;
/** icon 字段里像素图标的标记前缀，其后是种子。 */
export const PIXEL_PREFIX = "pixel:";

export interface Identicon {
  /** cells[row][col]，true 表示该格着色。 */
  cells: boolean[][];
  background: string;
  foreground: string;
}

/** xmur3 + 末位混淆：把任意字符串压成 32 位无符号序列。 */
function hashSeed(seed: string): () => number {
  let h = 1779033703 ^ seed.length;
  for (let i = 0; i < seed.length; i++) {
    h = Math.imul(h ^ seed.charCodeAt(i), 3432918353);
    h = (h << 13) | (h >>> 19);
  }
  return () => {
    h = Math.imul(h ^ (h >>> 16), 2246822507);
    h = Math.imul(h ^ (h >>> 13), 3266489909);
    return (h ^= h >>> 16) >>> 0;
  };
}

/** 生成 8 位十六进制种子。 */
export function pixelSeed(): string {
  let out = "";
  for (let i = 0; i < 8; i++) {
    out += Math.floor(Math.random() * 16).toString(16);
  }
  return out;
}

export function pixelIconValue(seed: string): string {
  return `${PIXEL_PREFIX}${seed}`;
}

export function createIdenticon(seed: string): Identicon {
  const next = hashSeed(seed || "agent");
  const hue = next() % 360;

  // 只生成左半 3 列，右侧镜像，保证左右对称。
  const half: boolean[] = [];
  for (let i = 0; i < 3 * IDENTICON_GRID; i++) {
    half.push(next() % 100 < 55);
  }

  const cells: boolean[][] = [];
  let filled = 0;
  for (let row = 0; row < IDENTICON_GRID; row++) {
    const cols: boolean[] = [];
    for (let col = 0; col < IDENTICON_GRID; col++) {
      const on = half[row * 3 + Math.min(col, IDENTICON_GRID - 1 - col)];
      cols.push(on);
      if (on) filled++;
    }
    cells.push(cols);
  }

  // 稀疏散列会认不出形状，中列补满兜底。
  if (filled < 7) {
    for (const cols of cells) cols[2] = true;
  }

  return {
    cells,
    // 同色相两档：最差色相（黄）下对比度仍 4.77:1，任何色相都看得清格子。
    background: `hsl(${hue} 44% 86%)`,
    foreground: `hsl(${hue} 58% 26%)`,
  };
}

export type AgentIconKind = "image" | "pixel" | "emoji";

export interface ResolvedAgentIcon {
  kind: AgentIconKind;
  /** image：URL；emoji：字符；pixel：种子。 */
  value: string;
  identicon?: Identicon;
}

/** 历史默认图标，等同于「未设置」。 */
export const LEGACY_DEFAULT_ICON = "🤖";

/**
 * 把 icon 字段解析成具体渲染方式。
 *
 * 上传图以 `/uploads/` 开头；`pixel:<seed>` 是像素图标；其余非空值当 emoji 用。
 * 空值与历史默认 🤖 都视为未设置，按 seed（通常是 Agent 名）生成像素图标。
 */
export function resolveAgentIcon(
  icon: string | undefined | null,
  seed?: string,
): ResolvedAgentIcon {
  const raw = (icon ?? "").trim();
  if (raw.startsWith("/uploads/")) return { kind: "image", value: raw };
  if (raw.startsWith(PIXEL_PREFIX)) {
    const s = raw.slice(PIXEL_PREFIX.length) || "agent";
    return { kind: "pixel", value: s, identicon: createIdenticon(s) };
  }
  if (raw && raw !== LEGACY_DEFAULT_ICON) {
    return { kind: "emoji", value: raw };
  }
  const s = seed?.trim() || "agent";
  return { kind: "pixel", value: s, identicon: createIdenticon(s) };
}
