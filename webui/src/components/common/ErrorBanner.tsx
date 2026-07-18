import { XCircle } from 'lucide-react'
import { Button } from '@/components/ui/button'

interface ErrorBannerProps {
  message: string
  onDismiss?: () => void
  onRetry?: () => void
}

export function ErrorBanner({ message, onDismiss, onRetry }: ErrorBannerProps) {
  return (
    <div className="flex items-center gap-3 rounded-md border border-destructive/30 bg-destructive/10 px-4 py-3">
      <XCircle className="h-5 w-5 text-destructive shrink-0" />
      <p className="flex-1 text-sm text-destructive">{message}</p>
      {onRetry && (
        <Button variant="outline" size="sm" onClick={onRetry}>重试</Button>
      )}
      {onDismiss && (
        <Button variant="ghost" size="sm" onClick={onDismiss}>×</Button>
      )}
    </div>
  )
}
