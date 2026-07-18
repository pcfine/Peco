import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import type { User } from '@/types/auth'
import * as authApi from '@/api/auth'

interface AuthState {
  user: User | null
  token: string | null
  isLoading: boolean
  error: string | null

  login: (email: string, password: string) => Promise<void>
  register: (username: string, email: string, password: string) => Promise<void>
  logout: () => void
  fetchMe: () => Promise<void>
  clearError: () => void
}

export const useAuthStore = create<AuthState>()(
  persist(
    (set, get) => ({
      user: null,
      token: null,
      isLoading: false,
      error: null,

      login: async (email, password) => {
        set({ isLoading: true, error: null })
        try {
          const res = await authApi.login({ email, password })
          set({ user: res.user, token: res.token, isLoading: false })
        } catch (e) {
          set({ isLoading: false, error: '邮箱或密码错误' })
          throw e
        }
      },

      register: async (username, email, password) => {
        set({ isLoading: true, error: null })
        try {
          const res = await authApi.register({ username, email, password })
          set({ user: res.user, token: res.token, isLoading: false })
        } catch (e) {
          set({ isLoading: false, error: '注册失败，请检查输入' })
          throw e
        }
      },

      logout: () => {
        set({ user: null, token: null, error: null })
      },

      fetchMe: async () => {
        const { token } = get()
        if (!token) return
        try {
          const user = await authApi.me()
          set({ user })
        } catch {
          // Token expired or invalid — clear auth state
          set({ user: null, token: null })
        }
      },

      clearError: () => set({ error: null }),
    }),
    {
      name: 'peco-auth',
      partialize: (state) => ({ token: state.token, user: state.user }),
    },
  ),
)
