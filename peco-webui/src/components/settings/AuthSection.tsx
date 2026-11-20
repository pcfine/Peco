// 设置页的账号区：改密码与退出登录。
//
// 退出登录只能靠服务端清 cookie —— 前端读不到 `peco_session`，本地删任何东西都不
// 等于登出，所以这里必须真发一次请求，成功后才翻会话态。

import { useEffect, useState, type FormEvent } from "react";
import { AuthField, AuthNotice, AuthSubmit } from "@/components/auth/AuthForm";
import { Button } from "@/components/ui/button";
import { changePassword, logout } from "@/api/auth";
import { ApiError, errorText } from "@/lib/api";
import { confirmPasswordError, newPasswordError } from "@/lib/authForm";
import { useAuthStore } from "@/stores/authStore";

const CURRENT_FIELD = "password-current";
const NEW_FIELD = "password-new";
const CONFIRM_FIELD = "password-confirm";

export function AuthSection() {
  const username = useAuthStore((state) => state.username);
  const markUnauthenticated = useAuthStore(
    (state) => state.markUnauthenticated,
  );
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [notice, setNotice] = useState<{
    tone: "error" | "success";
    text: string;
  } | null>(null);
  const [busy, setBusy] = useState(false);
  const [signingOut, setSigningOut] = useState(false);

  // 焦点在提交之后送：提交中字段是禁用的，禁用元素抢不到焦点。
  useEffect(() => {
    const firstInvalid = Object.keys(fieldErrors)[0];
    if (firstInvalid) document.getElementById(firstInvalid)?.focus();
  }, [fieldErrors]);

  async function onSignOut() {
    setSigningOut(true);
    setNotice(null);
    try {
      await logout();
      markUnauthenticated();
    } catch (error) {
      setNotice({ tone: "error", text: errorText(error) });
      setSigningOut(false);
    }
  }

  async function onChangePassword(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy) return;

    const nextErrors: Record<string, string> = {};
    if (current === "") nextErrors[CURRENT_FIELD] = "请填写当前密码";
    const passwordIssue = newPasswordError(next);
    if (passwordIssue) nextErrors[NEW_FIELD] = passwordIssue;
    const confirmIssue = confirmPasswordError(next, confirm);
    if (confirmIssue) nextErrors[CONFIRM_FIELD] = confirmIssue;

    setFieldErrors(nextErrors);
    if (Object.keys(nextErrors).length > 0) return;

    setBusy(true);
    setNotice(null);
    try {
      await changePassword({ current_password: current, new_password: next });
      setCurrent("");
      setNext("");
      setConfirm("");
      setNotice({ tone: "success", text: "密码已更新" });
    } catch (error) {
      if (error instanceof ApiError && error.code === "invalid_credentials") {
        setFieldErrors({ [CURRENT_FIELD]: "当前密码不对" });
      } else {
        setNotice({ tone: "error", text: errorText(error) });
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="space-y-2">
      <div className="flex h-8 items-center gap-2 border-b border-line">
        <h2 className="text-xs font-semibold text-ink-muted">账号</h2>
        <span className="min-w-0 truncate text-caption font-medium text-ink-faint">
          {username ? `当前登录：${username}` : "登录与密码"}
        </span>
      </div>

      <form
        onSubmit={onChangePassword}
        className="max-w-[360px] space-y-3"
        noValidate
      >
        <AuthField
          id={CURRENT_FIELD}
          name="current_password"
          label="当前密码"
          type="password"
          value={current}
          onChange={(event) => setCurrent(event.target.value)}
          autoComplete="current-password"
          error={fieldErrors[CURRENT_FIELD]}
          disabled={busy}
        />
        <AuthField
          id={NEW_FIELD}
          name="new_password"
          label="新密码"
          type="password"
          value={next}
          onChange={(event) => setNext(event.target.value)}
          autoComplete="new-password"
          error={fieldErrors[NEW_FIELD]}
          disabled={busy}
        />
        <AuthField
          id={CONFIRM_FIELD}
          name="confirm_password"
          label="确认新密码"
          type="password"
          value={confirm}
          onChange={(event) => setConfirm(event.target.value)}
          autoComplete="new-password"
          error={fieldErrors[CONFIRM_FIELD]}
          disabled={busy}
        />
        <AuthNotice message={notice?.text ?? null} tone={notice?.tone} />
        <AuthSubmit busy={busy}>修改密码</AuthSubmit>
      </form>

      <div className="flex max-w-[360px] items-center gap-2 border-t border-line pt-3">
        <Button
          type="button"
          variant="secondary"
          disabled={signingOut}
          aria-busy={signingOut}
          onClick={() => void onSignOut()}
        >
          退出登录
        </Button>
        <span className="min-w-0 truncate text-caption text-ink-faint">
          退出后需要重新登录才能使用
        </span>
      </div>
    </section>
  );
}
