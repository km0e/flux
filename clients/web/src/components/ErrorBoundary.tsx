/**
 * ErrorBoundary.tsx — catch rendering errors and show a fallback UI.
 *
 * React's error boundaries require a class component. Render errors are
 * logged (a bare setState swallows them) and the fallback offers a reset.
 *
 * Provides: ErrorBoundary
 */
import { Component } from 'react';
import { log } from '../logger';
import { Button } from './ui';

interface ErrorBoundaryProps {
  children: React.ReactNode;
}

interface ErrorBoundaryState {
  error: Error | null;
}

export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error): void {
    log.error('render error: ' + error.message + '\n' + (error.stack ?? ''));
  }

  private handleReset = (): void => {
    this.setState({ error: null });
  };

  render(): React.ReactNode {
    if (this.state.error) {
      return (
        <div className="flex h-full flex-col items-center justify-center gap-3 p-4">
          <div className="text-base font-semibold">Something went wrong</div>
          <div className="max-w-md text-center break-words text-xs text-muted">
            {this.state.error.message}
          </div>
          <Button variant="primary" size="sm" onClick={this.handleReset}>
            Try Again
          </Button>
        </div>
      );
    }
    return this.props.children;
  }
}
