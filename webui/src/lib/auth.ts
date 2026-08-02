import { create } from "zustand";
import type { User } from "@/types";
import { auth } from "./api";

interface AuthState {
  user: User | null;
  token: string | null;
  isLoading: boolean;
  isAuthenticated: boolean;
  login: (email: string, password: string) => Promise<void>;
  register: (
    username: string,
    email: string,
    password: string,
  ) => Promise<void>;
  logout: () => void;
  checkAuth: () => Promise<void>;
}

export const useAuth = create<AuthState>((set) => ({
  user: null,
  token: localStorage.getItem("token"),
  isLoading: true,
  isAuthenticated: false,

  login: async (email, password) => {
    const res = await auth.login({ email, password });
    localStorage.setItem("token", res.token);
    set({ user: res.user, token: res.token, isAuthenticated: true });
  },

  register: async (username, email, password) => {
    const res = await auth.register({ username, email, password });
    localStorage.setItem("token", res.token);
    set({ user: res.user, token: res.token, isAuthenticated: true });
  },

  logout: () => {
    localStorage.removeItem("token");
    set({ user: null, token: null, isAuthenticated: false });
  },

  checkAuth: async () => {
    const token = localStorage.getItem("token");
    if (!token) {
      set({ isLoading: false, isAuthenticated: false });
      return;
    }
    try {
      const user = await auth.me();
      set({ user, isAuthenticated: true, isLoading: false });
    } catch {
      localStorage.removeItem("token");
      set({
        user: null,
        token: null,
        isAuthenticated: false,
        isLoading: false,
      });
    }
  },
}));
