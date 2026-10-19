import { IDENTICON_GRID, type Identicon } from "@/lib/identicon";

/** 按 5×5 网格渲染像素图标，尺寸由 className 的宽高决定。 */
export function PixelIdenticon({
  identicon,
  className = "",
}: {
  identicon: Identicon;
  className?: string;
}) {
  const { cells, background, foreground } = identicon;
  return (
    <svg
      viewBox={`0 0 ${IDENTICON_GRID} ${IDENTICON_GRID}`}
      className={className}
      shapeRendering="crispEdges"
      aria-hidden="true"
      focusable="false"
    >
      <rect
        width={IDENTICON_GRID}
        height={IDENTICON_GRID}
        fill={background}
      />
      {cells.flatMap((row, y) =>
        row.map((on, x) =>
          on ? (
            <rect
              key={`${x}-${y}`}
              x={x}
              y={y}
              width={1}
              height={1}
              fill={foreground}
            />
          ) : null,
        ),
      )}
    </svg>
  );
}
