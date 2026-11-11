/** 5×5 镜像像素图标。 */

export const IDENTICON_GRID = 5;
/** 像素图标的标记前缀，其后是种子。 */
export const PIXEL_PREFIX = "pixel:";

export interface Identicon {
  /** cells[row][col]，true 表示该格着色。 */
  cells: boolean[][];
  background: string;
  foreground: string;
}

/** 将任意字符串压成 32 位无符号序列。 */
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

/** 由种子派生的稳定色相（0–359）：同一 id 永远同一色，刷新不变。 */
export function seedHue(seed: string): number {
  return hashSeed(seed || "agent")() % 360;
}

export function createIdenticon(seed: string): Identicon {
  const next = hashSeed(seed || "team");
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

  if (filled < 7) {
    for (const cols of cells) cols[2] = true;
  }

  return {
    cells,
    background: `hsl(${hue} 44% 86%)`,
    foreground: `hsl(${hue} 58% 26%)`,
  };
}

export type IconKind = "image" | "pixel" | "emoji";

export interface ResolvedIcon {
  kind: IconKind;
  /** image：URL；emoji：字符；pixel：种子。 */
  value: string;
  identicon?: Identicon;
}

/** 解析 icon 字段的渲染方式。 */
export function resolveIcon(
  icon: string | undefined | null,
  seed?: string,
): ResolvedIcon {
  const raw = (icon ?? "").trim();
  if (raw.startsWith("/uploads/")) return { kind: "image", value: raw };
  if (raw.startsWith(PIXEL_PREFIX)) {
    const s = raw.slice(PIXEL_PREFIX.length) || "team";
    return { kind: "pixel", value: s, identicon: createIdenticon(s) };
  }
  if (raw) return { kind: "emoji", value: raw };
  const s = seed?.trim() || "team";
  return { kind: "pixel", value: s, identicon: createIdenticon(s) };
}
