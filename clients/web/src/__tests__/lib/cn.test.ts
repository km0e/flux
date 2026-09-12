import { describe, it, expect } from 'vitest';
import { cn } from '../../lib/cn';

describe('cn', () => {
  it('joins multiple strings with a space', () => {
    expect(cn('a', 'b', 'c')).toBe('a b c');
  });

  it('filters out false values', () => {
    expect(cn('a', false, 'b', undefined, 'c', null)).toBe('a b c');
  });

  it('returns empty string when all args are falsy', () => {
    expect(cn(false, undefined, null, '')).toBe('');
  });

  it('returns empty string with no args', () => {
    expect(cn()).toBe('');
  });

  it('handles single string', () => {
    expect(cn('hello')).toBe('hello');
  });
});
