import { Component, type ReactNode } from 'react'
import { Button } from '@/components/ui/button'
import { AlertTriangle } from 'lucide-react'

interface Props {
  children: ReactNode
  fallback?: ReactNode
}

interface State {
  hasError: boolean
  error: Error | null
}

export class ErrorBoundary extends Component<Props, State> {
  constructor(props: Props) {
    super(props)
    this.state = { hasError: false, error: null }
  }

  static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error }
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    console.error('ErrorBoundary caught:', error, info.componentStack)
  }

  handleReset = () => {
    this.setState({ hasError: false, error: null })
  }

  render() {
    if (this.state.hasError) {
      if (this.props.fallback) return this.props.fallback

      return (
        <div className="flex min-h-[50vh] items-center justify-center p-8">
          <div className="text-center space-y-4 max-w-md">
            <AlertTriangle className="mx-auto h-12 w-12 text-destructive" />
            <h2 className="text-xl font-semibold">页面出错了</h2>
            <p className="text-sm text-muted-foreground">
              {this.state.error?.message ?? '发生了未知错误'}
            </p>
            <Button onClick={this.handleReset}>重新加载</Button>
          </div>
        </div>
      )
    }

    return this.props.children
  }
}
