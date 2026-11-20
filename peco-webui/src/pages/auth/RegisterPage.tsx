// 还没有账号时的闸门页 —— 未注册状态下它是默认首页。
//
// setup token 是「谁有权在这台机器上开账号」的唯一凭据：它只能在 daemon 的
// 机器上读到，所以文案要写清出处，不能只说「token 不对」。

import { useEffect, useRef, useState, type FormEvent } from "react";
import { AuthField, AuthNotice, AuthSubmit } from "@/components/auth/AuthForm";
import { AuthFrame } from "@/components/auth/AuthFrame";
import { register } from "@/api/auth";
import { ApiError, errorText } from "@/lib/api";
import {
  confirmPasswordError,
  newPasswordError,
  setupTokenError,
  usernameError,
} from "@/lib/authForm";
import { useAuthStore } from "@/stores/authStore";

const USERNAME_FIELD = "register-username";
const PASSWORD_FIELD = "register-password";
const CONFIRM_FIELD = "register-confirm";
const TOKEN_FIELD = "register-setup-token";

const SETUP_TOKEN_HINT = "在 daemon 机器上执行 cat ~/.peco-daemon/token";

export function RegisterPage() {
  const markAuthenticated = useAuthStore((state) => state.markAuthenticated);
  const checkStatus = useAuthStore((state) => state.checkStatus);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [setupToken, setSetupToken] = useState("");
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [formError, setFormError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const noticeRef = useRef<HTMLParagraphElement>(null);

  // 焦点在提交之后送：提交中字段是禁用的，禁用元素抢不到焦点。
  useEffect(() => {
    const firstInvalid = Object.keys(fieldErrors)[0];
    if (firstInvalid) document.getElementById(firstInvalid)?.focus();
  }, [fieldErrors]);

  useEffect(() => {
    if (formError) noticeRef.current?.focus();
  }, [formError]);

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy) return;

    const nextErrors: Record<string, string> = {};
    const usernameIssue = usernameError(username);
    if (usernameIssue) nextErrors[USERNAME_FIELD] = usernameIssue;
    const passwordIssue = newPasswordError(password);
    if (passwordIssue) nextErrors[PASSWORD_FIELD] = passwordIssue;
    const confirmIssue = confirmPasswordError(password, confirm);
    if (confirmIssue) nextErrors[CONFIRM_FIELD] = confirmIssue;
    const tokenIssue = setupTokenError(setupToken);
    if (tokenIssue) nextErrors[TOKEN_FIELD] = tokenIssue;

    setFieldErrors(nextErrors);
    if (Object.keys(nextErrors).length > 0) return;

    setBusy(true);
    setFormError(null);
    try {
      const identity = await register({
        username: username.trim(),
        password,
        setup_token: setupToken.trim(),
      });
      markAuthenticated(identity.username);
    } catch (error) {
      if (error instanceof ApiError && error.code === "invalid_setup_token") {
        setFieldErrors({
          [TOKEN_FIELD]:
            "setup token 不对，请在 daemon 机器上执行 cat ~/.peco-daemon/token",
        });
      } else if (
        error instanceof ApiError &&
        error.code === "already_registered"
      ) {
        // 本地这份「未注册」已经过期：重问状态，闸门会自己换成登录页。
        try {
          await checkStatus();
        } catch {
          setFormError("这台机器已经注册过账号，请刷新页面后登录");
        }
      } else {
        setFormError(errorText(error));
      }
      setBusy(false);
    }
  }

  return (
    <AuthFrame
      title="创建账号"
      description="这台机器上的 Peco 还没有账号。创建后即登录，密码至少 8 位。"
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
          autoComplete="new-password"
          error={fieldErrors[PASSWORD_FIELD]}
          disabled={busy}
        />
        <AuthField
          id={CONFIRM_FIELD}
          name="confirm_password"
          label="确认密码"
          type="password"
          value={confirm}
          onChange={(event) => setConfirm(event.target.value)}
          autoComplete="new-password"
          error={fieldErrors[CONFIRM_FIELD]}
          disabled={busy}
        />
        <AuthField
          id={TOKEN_FIELD}
          name="setup_token"
          label="setup token"
          value={setupToken}
          onChange={(event) => setSetupToken(event.target.value)}
          autoComplete="one-time-code"
          autoCapitalize="none"
          spellCheck={false}
          className="font-mono"
          hint={SETUP_TOKEN_HINT}
          error={fieldErrors[TOKEN_FIELD]}
          disabled={busy}
        />
        <AuthNotice ref={noticeRef} message={formError} />
        <AuthSubmit busy={busy}>创建账号</AuthSubmit>
      </form>
    </AuthFrame>
  );
}
