import axios from "axios";
import { useAuthStore } from "@/stores/authStore";
import { toast } from "sonner";

const api = axios.create({
  baseURL: "/api",
  timeout: 30_000,
});

// Request interceptor: attach Bearer token
api.interceptors.request.use((config) => {
  const token = useAuthStore.getState().token;
  if (token) {
    config.headers.Authorization = `Bearer ${token}`;
  }
  return config;
});

// Response interceptor: handle 401 / 429
api.interceptors.response.use(
  (response) => response,
  (error) => {
    if (error.response?.status === 401) {
      useAuthStore.getState().logout();
    } else if (error.response?.status === 429) {
      const retryAfter = error.response.headers["retry-after"];
      const msg = retryAfter
        ? `请求过于频繁，请在 ${retryAfter} 秒后重试`
        : "请求过于频繁，请稍后再试";
      toast.error(msg);
    }
    return Promise.reject(error);
  },
);

export default api;
