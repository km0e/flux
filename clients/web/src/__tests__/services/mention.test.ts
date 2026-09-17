/**
 * mention.test.ts — the @-path completion service: token extraction at
 * the caret, workdir-relative completion with directory drill-down, the
 * TTL'd directory cache (the agent creates files mid-session), and the
 * drop-path relativization.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import {
  mentionTokenAt,
  relativeToWorkdir,
  completeMention,
  _resetMentionForTest,
} from '../../services/mention';
import { listDir } from '../../services/fs';
import type { FsListing } from '../../core/types';

vi.mock('../../services/fs', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../services/fs')>()),
  listDir: vi.fn(),
}));

function listing(entries: Array<[string, 'dir' | 'file']>): FsListing {
  return {
    type: 'fs_listing',
    requested: '',
    entries: entries.map(([name, kind]) => ({ name, kind })),
  };
}

describe('mentionTokenAt', () => {
  it('finds the @-token under the caret', () => {
    expect(mentionTokenAt('see @src/li', 11)).toEqual({ start: 4, token: 'src/li' });
  });

  it('whitespace closes the segment; a mid-word @ is not a mention', () => {
    expect(mentionTokenAt('hello @world', 12)).toEqual({ start: 6, token: 'world' });
    // The @ must OPEN the segment — a mid-word @ is a literal character.
    expect(mentionTokenAt('a@b', 3)).toBeNull();
    expect(mentionTokenAt('hello world', 11)).toBeNull();
    expect(mentionTokenAt('done @src/x ', 12)).toBeNull(); // whitespace after the token
  });
});

describe('relativeToWorkdir', () => {
  it('strips the workdir prefix; outside paths stay absolute', () => {
    expect(relativeToWorkdir('/repo/src/a.ts', '/repo')).toBe('src/a.ts');
    expect(relativeToWorkdir('/repo', '/repo')).toBe('');
    expect(relativeToWorkdir('/elsewhere/x', '/repo')).toBe('/elsewhere/x');
    expect(relativeToWorkdir('/repo/src/', '/repo/')).toBe('src'); // trailing slashes normalize
  });
});

describe('completeMention', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    _resetMentionForTest();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('lists dirs first (trailing /) then files, workdir-relative', async () => {
    vi.mocked(listDir).mockResolvedValue(
      listing([
        ['src', 'dir'],
        ['readme.md', 'file'],
        ['notes', 'dir'],
      ]),
    );
    const items = await completeMention('/repo', '');
    expect(items).toEqual(['notes/', 'src/', 'readme.md']); // ALL dirs first, alphabetical, then files
    expect(vi.mocked(listDir)).toHaveBeenCalledWith('/repo');
  });

  it('drills into the token directory and filters by the base', async () => {
    vi.mocked(listDir).mockImplementation(async (path?: string) => {
      if (path === '/repo/src/lib') {
        return listing([
          ['dom.ts', 'file'],
          ['render.ts', 'file'],
          ['format.ts', 'file'],
        ]);
      }
      return listing([['lib', 'dir']]);
    });
    const items = await completeMention('/repo', 'src/lib/fo');
    expect(items).toEqual(['src/lib/format.ts']);
  });

  it('caches directories for the TTL — fresh listings win after it', async () => {
    vi.useFakeTimers();
    vi.mocked(listDir).mockResolvedValue(listing([['a.ts', 'file']]));
    await completeMention('/repo', '');
    await completeMention('/repo', ''); // cache hit
    expect(vi.mocked(listDir)).toHaveBeenCalledTimes(1);
    vi.setSystemTime(Date.now() + 16_000); // past the 15s TTL
    vi.mocked(listDir).mockResolvedValue(listing([['a.ts', 'file'], ['new.ts', 'file']]));
    await completeMention('/repo', '');
    expect(vi.mocked(listDir)).toHaveBeenCalledTimes(2);
    // The agent-created file IS completable after the refresh.
    expect(await completeMention('/repo', 'new')).toEqual(['new.ts']);
  });

  it('a failed listing resolves empty (no throw into the composer)', async () => {
    vi.mocked(listDir).mockResolvedValue({ type: 'fs_listing', requested: '/repo', error: 'boom', entries: [] });
    await expect(completeMention('/repo', '')).resolves.toEqual([]);
  });
});
