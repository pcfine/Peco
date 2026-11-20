// 已注册但未登录时的闸门页。凭据由 daemon 校验，前端不猜任何规则 ——
// 唯一要翻译的是三种结果：凭据不对、尝试过多、其它（含连不上）。

import { useEffect, useRef, useState, type FormEvent } from "react";
import { AuthField, AuthNotice, AuthSubmit } from "@/components/auth/AuthForm";
import { AuthFrame } from "@/components/auth/AuthFrame";
import { login } from "@/api/auth";
import { ApiError, errorText } from "@/lib/api";
import { useAuthStore } from "@/stores/authStore";

const USERNAME_FIELD = "login-username";
const PASSWORD_FIELD = "login-password";

/** 服务端没给冷却秒数时按 30 秒算：总比让用户连点再挨一次 429 强。 */
const DEFAULT_RETRY_SECONDS = 30;

export function LoginPage() {
  const markAuthenticated = useAuthStore((state) => state.markAuthenticated);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [formError, setFormError] = useState<string | null>(null);
  const [retryIn, setRetryIn] = useState(0);
  const [busy, setBusy] = useState(false);
  const noticeRef = useRef<HTMLParagraphElement>(null);

  useEffect(() => {
    if (retryIn <= 0) return;
    const timer = setTimeout(() => setRetryIn((seconds) => seconds - 1), 1000);
    return () => clearTimeout(timer);
  }, [retryIn]);

  // 焦点在提交之后送：提交中字段是禁用的，禁用元素抢不到焦点。
  useEffect(() => {
    const firstInvalid = Object.keys(fieldErrors)[0];
    if (firstInvalid) document.getElementById(firstInvalid)?.focus();
  }, [fieldErrors]);

  // 只在新错误落地时送焦点，倒计时每秒刷新文案不会跟着抢焦点。
  useEffect(() => {
    if (formError) noticeRef.current?.focus();
  }, [formError]);

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy || retryIn > 0) return;

    const trimmed = username.trim();
    const nextErrors: Record<string, string> = {};
    if (trimmed === "") nextErrors[USERNAME_FIELD] = "请填写用户名";
    if (password === "") nextErrors[PASSWORD_FIELD] = "请填写密码";
    setFieldErrors(nextErrors);
    if (Object.keys(nextErrors).length > 0) return;

    setBusy(true);
    setFormError(null);
    try {
      const identity = await login({ username: trimmed, password });
      // 成功后不复位提交态：闸门立刻换成应用，这一页不会再渲染。
      markAuthenticated(identity.username);
    } catch (error) {
      if (error instanceof ApiError && error.code === "invalid_credentials") {
        setFormError("用户名或密码不对");
        setPassword("");
      } else if (
        error instanceof ApiError &&
        error.code === "too_many_attempts"
      ) {
        setRetryIn(error.retryAfter ?? DEFAULT_RETRY_SECONDS);
      } else {
        setFormError(errorText(error));
      }
      setBusy(false);
    }
  }

  return (
    <AuthFrame
      title="登录"
      description="这台机器上的 Peco 需要账号密码，登录后即可进入。"
    >
      <form onSubmit={onSubmit} className="space-y-4" noValidate>
        <AuthField
          id={USERNAME_FIELD}
          name="username"
          label="用户名"
          value={username}
          onChange={(event) => setUsername(event.target.value)}
          autoComplete="username"
          autoCapitalize="none"
          spellCheck={false}
          error={fieldErrors[USERNAME_FIELD]}
          disabled={busy}
        />
        <AuthField
          id={PASSWORD_FIELD}
          name="password"
          label="密码"
          type="password"
          value={password}
          onChange={(event) => setPassword(event.target.value)}
          autoComplete="current-password"
          error={fieldErrors[PASSWORD_FIELD]}
          disabled={busy}
        />
        <AuthNotice
          ref={noticeRef}
          message={retryIn > 0 ? `尝试过多，请 ${retryIn} 秒后再试` : formError}
        />
        <AuthSubmit busy={busy} disabled={retryIn > 0}>
          登录
        </AuthSubmit>
      </form>
    </AuthFrame>
  );
}
