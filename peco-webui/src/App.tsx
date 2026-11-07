import { Navigate, Route, Routes } from "react-router-dom";
import { TooltipProvider } from "@/components/ui/tooltip";
import { AppShell } from "@/components/layout/AppShell";
import { NotFoundPage } from "@/pages/NotFoundPage";
import { PecoBoardPage } from "@/pages/PecoBoardPage";
import { ResultsPage } from "@/pages/ResultsPage";
import { SchedulesPage } from "@/pages/SchedulesPage";
import { SpaceDetailPage } from "@/pages/SpaceDetailPage";
import { NewSpacePage } from "@/pages/NewSpacePage";
import { SpaceListPage } from "@/pages/SpaceListPage";
import { TeamDetailPage } from "@/pages/TeamDetailPage";
import { TeamListPage } from "@/pages/TeamListPage";

/**
 * `/spaces/new` 是静态段，必须排在 `/spaces/:wsId` 之前：否则 `new` 会被当成 ws_id。
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
          <Route index element={<PecoBoardPage />} />
          <Route path="spaces" element={<SpaceListPage />} />
          <Route path="spaces/new" element={<NewSpacePage />} />
          <Route path="spaces/:wsId" element={<SpaceDetailPage />} />
          <Route path="teams" element={<TeamListPage />} />
          <Route path="teams/:teamId" element={<TeamDetailPage />} />
          <Route path="results" element={<ResultsPage />} />
          <Route path="schedules" element={<SchedulesPage />} />
          <Route path="home" element={<Navigate to="/" replace />} />
          <Route path="*" element={<NotFoundPage />} />
        </Route>
      </Routes>
    </TooltipProvider>
  );
}
