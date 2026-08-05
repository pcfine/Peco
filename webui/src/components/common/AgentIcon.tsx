// AgentIcon — renders agent icon as <img> for uploaded images or as emoji text.
// Uploaded icons start with "/uploads/"; everything else is treated as text/emoji.

interface AgentIconProps {
  icon: string;
  /** CSS background color (used behind emoji icons only) */
  backgroundColor?: string;
  /** Additional class names for the outer container */
  className?: string;
  /** Size variant: "sm" = w-6 h-6 text-xs, "lg" = w-16 h-16 text-2xl */
  size?: "sm" | "lg";
}

function isImageUrl(icon: string): boolean {
  return icon.startsWith("/uploads/");
}

export function AgentIcon({
  icon,
  backgroundColor = "#6366f1",
  className = "",
  size = "sm",
}: AgentIconProps) {
  const sizeClasses =
    size === "lg" ? "w-16 h-16 text-2xl" : "w-6 h-6 text-xs";
  const isImg = isImageUrl(icon);

  return (
    <div
      className={`rounded-full flex items-center justify-center shrink-0 overflow-hidden ${sizeClasses} ${className}`}
      style={{ backgroundColor: isImg ? "transparent" : backgroundColor }}
    >
      {isImg ? (
        <img
          src={icon}
          alt=""
          className="h-full w-full object-cover"
        />
      ) : (
        icon || "🤖"
      )}
    </div>
  );
}
