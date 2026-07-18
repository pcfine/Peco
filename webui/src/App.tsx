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
import { ChatListPage } from '@/pages/chat/ChatListPage'
import { ChatDetailPage } from '@/pages/chat/ChatDetailPage'
import { AgentListPage } from '@/pages/agents/AgentListPage'
import { AgentCreatePage } from '@/pages/agents/AgentCreatePage'
import { AgentEditPage } from '@/pages/agents/AgentEditPage'
import { KnowledgeListPage } from '@/pages/knowledge/KnowledgeListPage'
import { KnowledgeDetailPage } from '@/pages/knowledge/KnowledgeDetailPage'
import { TaskListPage } from '@/pages/tasks/TaskListPage'
import { TaskCreatePage } from '@/pages/tasks/TaskCreatePage'
import { TaskLogsPage } from '@/pages/tasks/TaskLogsPage'
import { SettingsPage } from '@/pages/settings/SettingsPage'

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
                  <Route path="/" element={<Navigate to="/chat" replace />} />
                  <Route path="/chat" element={<ChatListPage />} />
                  <Route path="/chat/:conversationId" element={<ChatDetailPage />} />
                  <Route path="/agents" element={<AgentListPage />} />
                  <Route path="/agents/new" element={<AgentCreatePage />} />
                  <Route path="/agents/:agentId/edit" element={<AgentEditPage />} />
                  <Route path="/knowledge" element={<KnowledgeListPage />} />
                  <Route path="/knowledge/:kbId" element={<KnowledgeDetailPage />} />
                  <Route path="/tasks" element={<TaskListPage />} />
                  <Route path="/tasks/new" element={<TaskCreatePage />} />
                  <Route path="/tasks/:taskId/logs" element={<TaskLogsPage />} />
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
