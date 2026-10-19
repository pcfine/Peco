// AgentIcon — 圆形头像：上传图 / 像素图标 / emoji 三种形态。

import { PixelIdenticon } from "@/components/common/PixelIdenticon";
import { resolveAgentIcon } from "@/lib/identicon";

interface AgentIconProps {
  icon: string;
  /** 未设置图标时的像素图标种子，通常传 Agent 名。 */
  name?: string;
  /** CSS background color (used behind emoji icons only) */
  backgroundColor?: string;
  /** Additional class names for the outer container */
  className?: string;
  /** Size variant: "sm" = w-6 h-6 text-xs, "lg" = w-16 h-16 text-2xl */
  size?: "sm" | "lg";
}

export function AgentIcon({
  icon,
  name,
  backgroundColor = "#6366f1",
  className = "",
  size = "sm",
}: AgentIconProps) {
  const sizeClasses = size === "lg" ? "w-16 h-16 text-2xl" : "w-6 h-6 text-xs";
  const resolved = resolveAgentIcon(icon, name);

  return (
    <div
      className={`rounded-full flex items-center justify-center shrink-0 overflow-hidden ${sizeClasses} ${className}`}
      style={{
        backgroundColor: resolved.kind === "emoji" ? backgroundColor : undefined,
      }}
    >
      {resolved.kind === "image" && (
        <img src={resolved.value} alt="" className="h-full w-full object-cover" />
      )}
      {resolved.kind === "pixel" && (
        <PixelIdenticon identicon={resolved.identicon!} className="h-full w-full" />
      )}
      {resolved.kind === "emoji" && (resolved.value || "🤖")}
    </div>
  );
}
