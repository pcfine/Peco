import { cn } from "@/lib/utils";

export function BrandMark({
  className,
  size = 28,
}: {
  className?: string;
  size?: number;
}) {
  return (
    <span
      className={cn(
        "grid shrink-0 place-items-center rounded-[0.55rem] bg-brand-gradient text-white shadow-sm shadow-primary/30",
        className,
      )}
      style={{ width: size, height: size }}
      aria-hidden="true"
    >
      <svg viewBox="0 0 24 24" fill="none" className="h-[60%] w-[60%]">
        <path
          d="M7.5 19.5V4.5h5.75a4.75 4.75 0 0 1 0 9.5H7.5"
          stroke="currentColor"
          strokeWidth={2.6}
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </svg>
    </span>
  );
}
