// 会话态是服务端事实的投影：`checkStatus` 落一次结果，之后靠拦截器与登录动作
// 翻面，不再各页各自探一遍。`checking` 初值为 true —— 首帧还没问过服务端时
// 既不能说「已登录」也不能说「未登录」，闸门要按「还不知道」渲染。

import { create } from "zustand";
import { getAuthStatus } from "@/api/auth";

interface AuthState {
  registered: boolean;
  authenticated: boolean;
  username: string | null;
  checking: boolean;
  /** 失败照常抛出：闸门据此区分「未登录」与「连不上 daemon」。 */
  checkStatus: () => Promise<void>;
  markUnauthenticated: () => void;
  markAuthenticated: (username: string) => void;
}

export const useAuthStore = create<AuthState>((set) => ({
  registered: false,
  authenticated: false,
  username: null,
  checking: true,
  checkStatus: async () => {
    set({ checking: true });
    try {
      const status = await getAuthStatus();
      set({
        registered: status.registered,
        authenticated: status.authenticated,
        username: status.username,
        checking: false,
      });
    } catch (error) {
      set({ checking: false });
      throw error;
    }
  },
  // 注册成功后本地还没有「已注册」的事实，但会话已生效 —— 用户名一起写进来，
  // 设置页才有东西可显示。
  markAuthenticated: (username) =>
    set({ registered: true, authenticated: true, username }),
  markUnauthenticated: () => set({ authenticated: false, username: null }),
}));
