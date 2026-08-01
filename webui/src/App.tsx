import { Suspense, lazy } from 'react'
import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom'
import { TooltipProvider } from '@/components/ui/tooltip'
import { Toaster } from 'sonner'
import { ErrorBoundary } from '@/components/ErrorBoundary'
import { LoadingSpinner } from '@/components/common/LoadingSpinner'
import { AppLayout } from '@/components/layout/AppLayout'
import { ProtectedRoute } from '@/components/ProtectedRoute'
import { LoginPage } from '@/pages/auth/LoginPage'
import { RegisterPage } from '@/pages/auth/RegisterPage'
import { PecoChatPage } from '@/pages/peco/PecoChatPage'
import { AgentChatPage } from '@/pages/chat/AgentChatPage'
import { AgentListPage } from '@/pages/agents/AgentListPage'
import { AgentCreatePage } from '@/pages/agents/AgentCreatePage'
import { AgentEditPage } from '@/pages/agents/AgentEditPage'
import { KnowledgeListPage } from '@/pages/knowledge/KnowledgeListPage'
import { KnowledgeDetailPage } from '@/pages/knowledge/KnowledgeDetailPage'
import { TaskListPage } from '@/pages/tasks/TaskListPage'
import { TaskCreatePage } from '@/pages/tasks/TaskCreatePage'
import { TaskLogsPage } from '@/pages/tasks/TaskLogsPage'
import { SettingsPage } from '@/pages/settings/SettingsPage'

// Lazy-loaded management pages (new in v2)
const ProviderListPage = lazy(() =>
  import('@/pages/manage/ProviderListPage').then((m) => ({ default: m.ProviderListPage })),
)
const SkillListPage = lazy(() =>
  import('@/pages/manage/SkillListPage').then((m) => ({ default: m.SkillListPage })),
)
const McpConfigPage = lazy(() =>
  import('@/pages/manage/McpConfigPage').then((m) => ({ default: m.McpConfigPage })),
)

export default function App() {
  return (
    <TooltipProvider>
      <ErrorBoundary>
        <Suspense fallback={<LoadingSpinner className="min-h-screen" />}>
          <BrowserRouter>
            <Routes>
              <Route path="/login" element={<LoginPage />} />
              <Route path="/register" element={<RegisterPage />} />
              <Route element={<ProtectedRoute />}>
                <Route element={<AppLayout />}>
                  {/* Redirect root to Peco */}
                  <Route path="/" element={<Navigate to="/peco" replace />} />
                  <Route path="/chat" element={<Navigate to="/peco" replace />} />

                  {/* Peco 永续聊天 */}
                  <Route path="/peco" element={<PecoChatPage />} />

                  {/* Agent 对话 */}
                  <Route path="/chat/:agentId" element={<AgentChatPage />} />
                  <Route path="/chat/:agentId/:conversationId" element={<AgentChatPage />} />

                  {/* 管理 */}
                  <Route path="/manage/providers" element={<ProviderListPage />} />
                  <Route path="/manage/agents" element={<AgentListPage />} />
                  <Route path="/manage/agents/new" element={<AgentCreatePage />} />
                  <Route path="/manage/agents/:agentId/edit" element={<AgentEditPage />} />
                  <Route path="/manage/skills" element={<SkillListPage />} />
                  <Route path="/manage/mcp" element={<McpConfigPage />} />
                  <Route path="/manage/knowledge" element={<KnowledgeListPage />} />
                  <Route path="/manage/knowledge/:kbId" element={<KnowledgeDetailPage />} />

                  {/* 任务 */}
                  <Route path="/tasks" element={<TaskListPage />} />
                  <Route path="/tasks/new" element={<TaskCreatePage />} />
                  <Route path="/tasks/:taskId/logs" element={<TaskLogsPage />} />

                  {/* 设置 */}
                  <Route path="/settings" element={<SettingsPage />} />
                </Route>
              </Route>
            </Routes>
          </BrowserRouter>
        </Suspense>
      </ErrorBoundary>
      <Toaster position="top-right" richColors />
    </TooltipProvider>
  )
}
