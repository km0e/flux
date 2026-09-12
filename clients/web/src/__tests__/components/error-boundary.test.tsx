/**
 * error-boundary.test.tsx — render errors surface a fallback, not a blank page.
 */
import { describe, it, expect, vi } from 'vitest';
import { render, fireEvent, screen } from '@testing-library/react';
import { ErrorBoundary } from '../../components/ErrorBoundary';

function Bomb(): React.ReactElement {
  throw new Error('kaboom');
}

describe('ErrorBoundary', () => {
  it('catches render errors and offers a reset', () => {
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
    function Flippable({ bomb }: { bomb: boolean }) {
      return (
        <ErrorBoundary>{bomb ? <Bomb /> : <div>fine</div>}</ErrorBoundary>
      );
    }
    const { rerender } = render(<Flippable bomb={true} />);
    expect(screen.getByText('Something went wrong')).toBeTruthy();
    expect(screen.getByText(/kaboom/)).toBeTruthy();
    // Reset recovers once the child stops throwing.
    rerender(<Flippable bomb={false} />);
    fireEvent.click(screen.getByRole('button', { name: 'Try Again' }));
    expect(screen.getByText('fine')).toBeTruthy();
    spy.mockRestore();
  });

  it('renders children untouched when nothing throws', () => {
    render(
      <ErrorBoundary>
        <div>all good</div>
      </ErrorBoundary>,
    );
    expect(screen.getByText('all good')).toBeTruthy();
  });
});
