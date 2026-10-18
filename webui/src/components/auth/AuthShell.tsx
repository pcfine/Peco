import type { ReactNode } from "react";
import { BrandMark } from "@/components/common/BrandMark";

interface AuthShellProps {
  title: string;
  description?: string;
  children: ReactNode;
  footer?: ReactNode;
}

export function AuthShell({
  title,
  description,
  children,
  footer,
}: AuthShellProps) {
  return (
    <div className="relative flex min-h-screen bg-background">
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-0 overflow-hidden"
      >
        <div className="absolute -top-48 -left-40 h-[36rem] w-[36rem] rounded-full bg-primary/10 blur-[130px]" />
        <div className="absolute -bottom-40 right-[-6rem] h-[30rem] w-[30rem] rounded-full bg-info/10 blur-[130px]" />
      </div>

      <div className="relative hidden w-1/2 flex-col justify-between p-12 lg:flex">
        <div className="flex items-center gap-2.5">
          <BrandMark size={32} />
          <span className="text-lg font-semibold tracking-tight">Peco</span>
        </div>
        <div className="max-w-md space-y-4">
          <h2 className="text-[2rem] leading-tight font-semibold tracking-tight text-balance">
            你的 AI Agent 工作空间
          </h2>
          <p className="text-muted-foreground">
            永续对话、长期记忆、可编排的工作流与知识库 —— 收敛在一个克制的界面里。
          </p>
        </div>
        <p className="text-xs text-muted-foreground">© Peco · 本地部署</p>
      </div>

      <div className="relative flex w-full items-center justify-center px-4 lg:w-1/2">
        <div className="w-full max-w-sm">
          <div className="mb-8 flex items-center gap-2.5 lg:hidden">
            <BrandMark size={32} />
            <span className="text-lg font-semibold tracking-tight">Peco</span>
          </div>
          <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
          {description && (
            <p className="mt-1.5 text-sm text-muted-foreground">
              {description}
            </p>
          )}
          <div className="mt-8">{children}</div>
          {footer && (
            <div className="mt-6 text-center text-sm text-muted-foreground">
              {footer}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
