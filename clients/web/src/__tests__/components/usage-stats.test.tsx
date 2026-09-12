/**
 * usage-stats.test.tsx — token usage formatting (compact ↑/↓/R/W language).
 */
import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { UsageStats, fmtTokens } from '../../components/UsageStats';
import type { UsageTotals } from '../../core/state';

const TOTALS: UsageTotals = {
  inTokens: 12_345,
  outTokens: 999,
  cachedTokens: 1000,
  contextTokens: 12_345,
};

describe('fmtTokens', () => {
  it('formats compactly', () => {
    expect(fmtTokens(999)).toBe('999');
    expect(fmtTokens(1000)).toBe('1k');
    expect(fmtTokens(12_345)).toBe('12.3k');
    expect(fmtTokens(123_456)).toBe('123k');
    expect(fmtTokens(1_500_000)).toBe('1.5m');
  });
});

describe('UsageStats', () => {
  it('renders the four stat units (↑ ↓ R W)', () => {
    const { container } = render(<UsageStats usage={TOTALS} />);
    expect(container.querySelector('#usage-stats')).toBeTruthy();
    expect(screen.getByText('12.3k')).toBeTruthy(); // input
    expect(screen.getByText('999')).toBeTruthy(); // output
    expect(screen.getByText('1k')).toBeTruthy(); // cache read
  });

  it('hides the cache-read unit when nothing was cached', () => {
    render(
      <UsageStats
        usage={{ inTokens: 100, outTokens: 50, cachedTokens: 0, contextTokens: 100 }}
      />,
    );
    expect(screen.queryByText('R')).toBeNull();
  });

  it('renders nothing without usage', () => {
    const { container } = render(<UsageStats usage={undefined} />);
    expect(container.querySelector('#usage-stats')).toBeNull();
  });
});
