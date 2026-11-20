// 应用与登录之间只有这一道判断。
//
// 首帧必须停在「还不知道」：先渲染应用再跳登录，闪的那一下里各页已经在打自己的
// 接口，401 会连环弹出；先渲染登录页又会在已登录时闪一下。所以拿到第一个结论
// 之前只给一个静态占位，等 `/auth/status` 回话再决定渲染哪一边。

import { useCallback, useEffect, useState, type ReactNode } from "react";
import { AuthFrame } from "@/components/auth/AuthFrame";
import { Button } from "@/components/ui/button";
import { LoginPage } from "@/pages/auth/LoginPage";
import { RegisterPage } from "@/pages/auth/RegisterPage";
import { errorText } from "@/lib/api";
import { useAuthStore } from "@/stores/authStore";

export function AuthGate({ children }: { children: ReactNode }) {
  const registered = useAuthStore((state) => state.registered);
  const authenticated = useAuthStore((state) => state.authenticated);
  const checking = useAuthStore((state) => state.checking);
  const checkStatus = useAuthStore((state) => state.checkStatus);
  const [statusError, setStatusError] = useState<string | null>(null);

  const check = useCallback(async () => {
    setStatusError(null);
    try {
      await checkStatus();
    } catch (error) {
      setStatusError(errorText(error));
    }
  }, [checkStatus]);

  useEffect(() => {
    void check();
  }, [check]);

  // 只有「还没问过服务端，手里也一无所知」才给占位。重新核对时本地已有结论，
  // 把已经渲染好的应用卸掉再挂回来会让整棵树重建（对话、面板状态全丢）。
  if (checking && !registered && !authenticated) return <AuthChecking />;

  // 连不上 daemon 时不能当成「未登录」：那会把登录页变成一条死路。
  if (statusError) {
    return (
      <AuthFrame title="无法确认登录状态" description={statusError}>
        <Button className="w-full" onClick={() => void check()}>
          重试
        </Button>
      </AuthFrame>
    );
  }

  if (authenticated) return <>{children}</>;
  if (!registered) return <RegisterPage />;
  return <LoginPage />;
}

function AuthChecking() {
  return (
    <div
      className="grid h-full place-items-center bg-canvas px-4"
      role="status"
      aria-live="polite"
    >
      <p className="text-sm text-ink-faint">正在确认登录状态…</p>
    </div>
  );
}
