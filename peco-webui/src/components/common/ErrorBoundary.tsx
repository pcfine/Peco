import { Component, type ReactNode } from "react";
import { AlertTriangle } from "lucide-react";
import { Button } from "@/components/ui/button";

interface Props {
  children: ReactNode;
  fallback?: ReactNode;
}

interface State {
  hasError: boolean;
  error: Error | null;
}

/** 局部兜底：预览渲染失败时只塌这一块，源码编辑不受影响。 */
export class ErrorBoundary extends Component<Props, State> {
  constructor(props: Props) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    console.error("ErrorBoundary caught:", error, info.componentStack);
  }

  handleReset = () => {
    this.setState({ hasError: false, error: null });
  };

  render() {
    if (this.state.hasError) {
      if (this.props.fallback) return this.props.fallback;

      return (
        <div className="flex min-h-[50vh] items-center justify-center p-8">
          <div className="max-w-md space-y-4 text-center">
            <AlertTriangle className="mx-auto size-12 text-danger" />
            <h2 className="text-lg font-semibold text-ink">页面出错了</h2>
            <p className="text-sm text-ink-muted">
              {this.state.error?.message ?? "发生了未知错误"}
            </p>
            <Button onClick={this.handleReset}>重新加载</Button>
          </div>
        </div>
      );
    }

    return this.props.children;
  }
}
