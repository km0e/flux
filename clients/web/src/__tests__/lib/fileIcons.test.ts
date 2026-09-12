/**
 * fileIcons.test.ts — extension/name → glyph/color mapping.
 *
 * Provides: fileIcon tests
 * Depends: lib/fileIcons
 */
import { describe, it, expect } from 'vitest';
import { fileIcon } from '../../lib/fileIcons';

describe('fileIcon', () => {
  it('maps known extensions to their language glyph and color', () => {
    expect(fileIcon('main.ts')).toMatchObject({ glyph: 'TS' });
    expect(fileIcon('lib.rs')).toMatchObject({ glyph: 'RS' });
    expect(fileIcon('app.py')).toMatchObject({ glyph: 'PY' });
    expect(fileIcon('server.go')).toMatchObject({ glyph: 'GO' });
    expect(fileIcon('README.md')).toMatchObject({ glyph: 'MD' });
    expect(fileIcon('style.css')).toMatchObject({ glyph: '#' });
  });

  it('maps whole names over extensions (Dockerfile, Cargo.toml, lock files)', () => {
    expect(fileIcon('Dockerfile')).toMatchObject({ glyph: 'DK' });
    expect(fileIcon('Cargo.toml')).toMatchObject({ glyph: 'RS', color: fileIcon('a.rs').color });
    expect(fileIcon('Cargo.lock')).toMatchObject({ glyph: 'LCK' });
    expect(fileIcon('Makefile')).toMatchObject({ glyph: 'MK' });
    expect(fileIcon('LICENSE')).toMatchObject({ glyph: '©' });
  });

  it('handles git and env families by prefix', () => {
    expect(fileIcon('.gitignore')).toMatchObject({ glyph: 'GIT' });
    expect(fileIcon('.gitattributes')).toMatchObject({ glyph: 'GIT' });
    expect(fileIcon('.env')).toMatchObject({ glyph: 'ENV' });
    expect(fileIcon('.env.local')).toMatchObject({ glyph: 'ENV' });
  });

  it('falls back to a ≤3-char uppercase extension glyph in gray', () => {
    const g = fileIcon('archive.tarball');
    expect(g.glyph).toBe('TAR');
    expect(g.color).toBe(fileIcon('unknown.xyz').color);
  });

  it('falls back to a text-lines glyph for extension-less files', () => {
    expect(fileIcon('binary').glyph).toBe('≡');
  });

  it('is case-insensitive', () => {
    expect(fileIcon('DOCKERFILE')).toMatchObject(fileIcon('dockerfile'));
    expect(fileIcon('PHOTO.JPG')).toMatchObject({ glyph: 'IMG' });
  });
});
