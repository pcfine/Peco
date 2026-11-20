// 账号密码认证：daemon 只认 `peco_session` 这个 HttpOnly cookie，前端读不到它，
// 所以「谁登录了」只能问 `/auth/status`，不能从本地存储推。

import { api } from "@/lib/api";

export interface AuthStatus {
  /** 这台机器上有没有账号。没有时前端默认把注册页摆在最前。 */
  registered: boolean;
  authenticated: boolean;
  username: string | null;
}

export interface AuthIdentity {
  username: string;
}

export interface RegisterBody {
  username: string;
  password: string;
  setup_token: string;
}

export interface LoginBody {
  username: string;
  password: string;
}

export interface ChangePasswordBody {
  current_password: string;
  new_password: string;
}

export async function getAuthStatus(): Promise<AuthStatus> {
  const res = await api.get<AuthStatus>("/auth/status");
  return res.data;
}

/** 成功即带上 Set-Cookie：会话由 cookie 承载，返回值里只有用户名。 */
export async function login(body: LoginBody): Promise<AuthIdentity> {
  const res = await api.post<AuthIdentity>("/auth/login", body);
  return res.data;
}

export async function register(body: RegisterBody): Promise<AuthIdentity> {
  const res = await api.post<AuthIdentity>("/auth/register", body);
  return res.data;
}

export async function logout(): Promise<void> {
  await api.post("/auth/logout");
}

export async function changePassword(body: ChangePasswordBody): Promise<void> {
  await api.post("/auth/password", body);
}
