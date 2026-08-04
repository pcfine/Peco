import { useAuthStore } from "@/stores/authStore";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { ProviderSection } from "@/components/settings/ProviderSection";
import { useNavigate } from "react-router-dom";

export function SettingsPage() {
  const { user, logout } = useAuthStore();
  const navigate = useNavigate();

  const handleLogout = () => {
    logout();
    navigate("/login");
  };

  return (
    <div className="max-w-2xl mx-auto space-y-6">
      <h2 className="text-2xl font-bold">设置</h2>

      <Card>
        <CardHeader>
          <CardTitle>用户信息</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="flex items-center gap-4">
            <Avatar className="h-16 w-16">
              <AvatarFallback className="text-lg">
                {user?.username.slice(0, 2).toUpperCase()}
              </AvatarFallback>
            </Avatar>
            <div>
              <p className="text-lg font-semibold">{user?.username}</p>
              <p className="text-sm text-muted-foreground">{user?.email}</p>
              <p className="text-xs text-muted-foreground">
                注册时间:{" "}
                {user?.created_at
                  ? new Date(user.created_at).toLocaleDateString("zh-CN")
                  : "-"}
              </p>
            </div>
          </div>
          <Button variant="destructive" onClick={handleLogout}>
            退出登录
          </Button>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Provider 配置</CardTitle>
        </CardHeader>
        <CardContent>
          <ProviderSection />
        </CardContent>
      </Card>
    </div>
  );
}
