// 账号表单的三个零件：字段、提交按钮、表单级提示。登录页、注册页与设置页的
// 修改密码区都用它们，错误文案与焦点行为只在这里定义一次。

import type { ComponentProps } from "react";
import { Loader2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";

interface AuthFieldProps extends ComponentProps<"input"> {
  id: string;
  label: string;
  /** 字段下方的常驻说明（例如 setup token 从哪来）。 */
  hint?: string;
  error?: string | null;
}

export function AuthField({
  id,
  label,
  hint,
  error,
  className,
  ...inputProps
}: AuthFieldProps) {
  const hintId = hint ? `${id}-hint` : undefined;
  const errorId = error ? `${id}-error` : undefined;
  const describedBy = [hintId, errorId].filter(Boolean).join(" ") || undefined;

  return (
    <div className="space-y-1">
      <Label htmlFor={id}>{label}</Label>
      <Input
        id={id}
        aria-invalid={error ? true : undefined}
        aria-describedby={describedBy}
        className={className}
        {...inputProps}
      />
      {hint && (
        <p id={hintId} className="text-caption text-ink-faint">
          {hint}
        </p>
      )}
      {error && (
        <p id={errorId} className="text-caption font-medium text-danger">
          {error}
        </p>
      )}
    </div>
  );
}

interface AuthSubmitProps {
  busy: boolean;
  disabled?: boolean;
  children: string;
}

/** 提交中保留原标题，只外加一枚转圈：按钮名字变了会让读屏与用户都失去锚点。 */
export function AuthSubmit({
  busy,
  disabled = false,
  children,
}: AuthSubmitProps) {
  return (
    <Button
      type="submit"
      className="w-full"
      disabled={busy || disabled}
      aria-busy={busy}
    >
      {busy && (
        <Loader2 aria-hidden="true" className="motion-safe:animate-spin" />
      )}
      {children}
    </Button>
  );
}

interface AuthNoticeProps extends ComponentProps<"p"> {
  message: string | null;
  tone?: "error" | "success";
}

/** 表单级提示（凭据不对、密码已更新）：落不到具体字段上的那类。 */
export function AuthNotice({
  message,
  tone = "error",
  className,
  ...props
}: AuthNoticeProps) {
  if (!message) return null;
  return (
    <p
      tabIndex={-1}
      role={tone === "error" ? "alert" : "status"}
      className={cn(
        "text-sm font-medium",
        tone === "error" ? "text-danger" : "text-ok",
        className,
      )}
      {...props}
    >
      {message}
    </p>
  );
}
