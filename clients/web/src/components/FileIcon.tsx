/**
 * FileIcon.tsx — small presentational file/folder icons shared by the
 * Explorer tree and the preview header.
 *
 * Files render as a tinted monospace glyph chip (colors from lib/fileIcons,
 * a fixed decorative palette — language identity never drifts with
 * the theme); folders render as an inline SVG that swaps to its open variant
 * when expanded. Both are decorative (aria-hidden) — the name carries the
 * semantics.
 *
 * Provides: FileIcon
 * Depends: lib/fileIcons.ts
 */
import { fileIcon } from '../lib/fileIcons';

/** Tinted glyph chip for a file name (16×16). The 8px glyph is a deliberate
 * type-scale exception — this is an ICON, not text: two-letter language
 * codes (TS, PY…) cannot clear 11px inside a 16px tile. */
export function FileIcon({ name }: { name: string }): React.ReactElement {
  const { glyph, color } = fileIcon(name);
  return (
    <span
      aria-hidden="true"
      className="inline-grid size-4 shrink-0 place-items-center rounded-xs font-mono text-[8px] leading-none font-bold tracking-[-0.02em]"
      style={{
        color,
        background: `color-mix(in srgb, ${color} 16%, transparent)`,
      }}
    >
      {glyph}
    </span>
  );
}
