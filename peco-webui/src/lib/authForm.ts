// 账号表单的校验规则：注册页、登录页与设置页的「修改密码」共用同一套数字，
// 各写一份就会在某一页上悄悄放宽。纯函数，文案即承诺，可单测。

export const PASSWORD_MIN = 8;
export const PASSWORD_MAX = 128;
export const USERNAME_MAX = 64;

/** 用户名去空白后 1..64：去空白是因为粘贴常带尾随空格。 */
export function usernameError(username: string): string | null {
  const value = username.trim();
  if (value === "") return "用户名不能为空";
  if (value.length > USERNAME_MAX) {
    return `用户名不能超过 ${USERNAME_MAX} 个字符`;
  }
  return null;
}

export function newPasswordError(password: string): string | null {
  if (password.length < PASSWORD_MIN || password.length > PASSWORD_MAX) {
    return `密码要 ${PASSWORD_MIN} 到 ${PASSWORD_MAX} 个字符`;
  }
  return null;
}

export function confirmPasswordError(
  password: string,
  confirm: string,
): string | null {
  if (password !== confirm) return "两次输入的密码不一致";
  return null;
}

export function setupTokenError(token: string): string | null {
  if (token.trim() === "") return "请粘贴 setup token";
  return null;
}
