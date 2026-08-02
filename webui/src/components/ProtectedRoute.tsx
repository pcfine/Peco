import { Navigate, Outlet } from "react-router-dom";
import { useAuthStore } from "@/stores/authStore";
import { useEffect } from "react";

export function ProtectedRoute() {
  const { token, user, fetchMe } = useAuthStore();

  useEffect(() => {
    if (token && !user) {
      fetchMe();
    }
  }, [token, user, fetchMe]);

  if (!token) {
    return <Navigate to="/login" replace />;
  }

  return <Outlet />;
}
