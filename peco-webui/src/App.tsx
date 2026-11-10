import { Navigate, Route, Routes } from "react-router-dom";
import { TooltipProvider } from "@/components/ui/tooltip";
import { AppShell } from "@/components/layout/AppShell";
import { NotFoundPage } from "@/pages/NotFoundPage";
import { PecoChatPage } from "@/pages/PecoChatPage";
import { ResultsPage } from "@/pages/ResultsPage";
import { SchedulesPage } from "@/pages/SchedulesPage";
import { SettingsPage } from "@/pages/SettingsPage";
import { SpaceDetailPage } from "@/pages/SpaceDetailPage";
import { SpaceStatusPage } from "@/pages/SpaceStatusPage";
import { NewSpacePage } from "@/pages/NewSpacePage";
import { SpaceListPage } from "@/pages/SpaceListPage";
import { TeamDetailPage } from "@/pages/TeamDetailPage";
import { TeamListPage } from "@/pages/TeamListPage";

/**
 * `/` 只剩一条重定向：Peco 有自己的路由，不再借用首页。
 *
 * Peco 的能力配置走左头的 `·peco` 链接：`/teams/peco` 与其它团队共用
 * 同一份配置页。
 *
 * `/spaces/new` 与 `/spaces/:wsId/status` 是静态段收尾/追加，必须能与 `/spaces/:wsId`
 * 区分开：`new` 不能被当成 ws_id，`status` 不能被当成 ws_id 的一部分。
 *
 * `TooltipProvider` 挂在路由之上：`ChatView` 的 `<Tooltip>` 是无条件使用的，
 * 缺 provider 时 Radix 抛错会连坐卸载整棵树（首屏与空间页全白）。放在根部只有一份，
 * 调用点不必各自包一层。
 */
export function App() {
  return (
    <TooltipProvider>
      <Routes>
        <Route element={<AppShell />}>
          <Route index element={<Navigate to="/peco" replace />} />
          <Route path="peco" element={<PecoChatPage />} />
          <Route path="spaces" element={<SpaceListPage />} />
          <Route path="spaces/new" element={<NewSpacePage />} />
          <Route path="spaces/:wsId" element={<SpaceDetailPage />} />
          <Route path="spaces/:wsId/status" element={<SpaceStatusPage />} />
          <Route path="teams" element={<TeamListPage />} />
          <Route path="teams/:teamId" element={<TeamDetailPage />} />
          <Route path="results" element={<ResultsPage />} />
          <Route path="schedules" element={<SchedulesPage />} />
          <Route path="settings" element={<SettingsPage />} />
          <Route path="home" element={<Navigate to="/peco" replace />} />
          <Route path="*" element={<NotFoundPage />} />
        </Route>
      </Routes>
    </TooltipProvider>
  );
}
