/**
 * fileIcons.ts — extension/name → { glyph, color } for tree rows and the
 * preview header. Text glyphs in a tinted chip (vscode-seti spirit) keep the
 * UI dependency-free while giving files instant visual identity.
 *
 * Colors are FIXED decorative constants (brand-like language colors, same on
 * every theme — GitHub linguist does the same). They are intentionally NOT
 * --fx-* tokens: the token contract is for semantic surface colors, and
 * language identity must not shift with the host theme. All values are
 * medium-tone so they read on both light and dark backgrounds.
 *
 * Provides: fileIcon (pure), FileIconGlyph type
 * Depends: nothing
 */

export interface FileIconGlyph {
  /** 1–3 char monospace glyph shown in the chip. */
  glyph: string;
  /** Decorative language color (fixed, theme-independent). */
  color: string;
}

/** Language color palette — medium tones legible on light AND dark. */
const C = {
  blue: '#3178c6', // TypeScript
  amber: '#c48a04', // JavaScript
  olive: '#8f9600', // JSON
  rust: '#c1502e', // Rust
  python: '#3572a5',
  go: '#0085ad',
  teal: '#43899c', // Markdown
  steel: '#6d86ad', // C/C++
  green: '#1f8a3c', // C#
  java: '#b07219',
  ruby: '#cc3433',
  php: '#8993be',
  shell: '#4e9a2f',
  html: '#e34c26',
  vue: '#41b883',
  svelte: '#e2543e',
  css: '#563d7c',
  scss: '#c6538c',
  yaml: '#b54545',
  sql: '#c07f1d',
  image: '#a074c4',
  font: '#b0835f',
  archive: '#b08d3f',
  pdf: '#d13438',
  csv: '#3f9e7d',
  video: '#c4558f',
  git: '#e8563f',
  env: '#6f9e3d',
  docker: '#2496ed',
  gray: '#8a9199',
} as const;

const EXT_MAP: Record<string, FileIconGlyph> = {
  // TypeScript / JavaScript
  ts: { glyph: 'TS', color: C.blue },
  tsx: { glyph: 'TS', color: C.blue },
  mts: { glyph: 'TS', color: C.blue },
  cts: { glyph: 'TS', color: C.blue },
  js: { glyph: 'JS', color: C.amber },
  jsx: { glyph: 'JS', color: C.amber },
  mjs: { glyph: 'JS', color: C.amber },
  cjs: { glyph: 'JS', color: C.amber },
  json: { glyph: '{}', color: C.olive },
  jsonc: { glyph: '{}', color: C.olive },
  json5: { glyph: '{}', color: C.olive },

  // Systems languages
  rs: { glyph: 'RS', color: C.rust },
  go: { glyph: 'GO', color: C.go },
  c: { glyph: 'C', color: C.steel },
  h: { glyph: 'C', color: C.steel },
  cpp: { glyph: 'C+', color: C.steel },
  cc: { glyph: 'C+', color: C.steel },
  cxx: { glyph: 'C+', color: C.steel },
  hpp: { glyph: 'C+', color: C.steel },
  hh: { glyph: 'C+', color: C.steel },
  cs: { glyph: 'C#', color: C.green },
  java: { glyph: 'JV', color: C.java },
  rb: { glyph: 'RB', color: C.ruby },
  php: { glyph: 'PHP', color: C.php },
  swift: { glyph: 'SW', color: C.java },
  kt: { glyph: 'KT', color: C.php },
  lua: { glyph: 'LUA', color: C.go },
  zig: { glyph: 'ZG', color: C.amber },
  hs: { glyph: 'HS', color: C.php },
  ex: { glyph: 'EX', color: C.php },
  dart: { glyph: 'DA', color: C.go },

  // Web
  py: { glyph: 'PY', color: C.python },
  pyi: { glyph: 'PY', color: C.python },
  html: { glyph: '<>', color: C.html },
  htm: { glyph: '<>', color: C.html },
  vue: { glyph: 'V', color: C.vue },
  svelte: { glyph: 'SV', color: C.svelte },
  css: { glyph: '#', color: C.css },
  scss: { glyph: '#', color: C.scss },
  sass: { glyph: '#', color: C.scss },
  less: { glyph: '#', color: C.css },
  md: { glyph: 'MD', color: C.teal },
  mdx: { glyph: 'MD', color: C.teal },
  markdown: { glyph: 'MD', color: C.teal },

  // Data / config
  yml: { glyph: 'YML', color: C.yaml },
  yaml: { glyph: 'YML', color: C.yaml },
  toml: { glyph: 'TOM', color: C.gray },
  ini: { glyph: 'INI', color: C.gray },
  cfg: { glyph: 'CFG', color: C.gray },
  conf: { glyph: 'CNF', color: C.gray },
  sql: { glyph: 'SQL', color: C.sql },
  csv: { glyph: 'CSV', color: C.csv },
  tsv: { glyph: 'CSV', color: C.csv },
  xml: { glyph: 'XML', color: C.go },
  txt: { glyph: 'TXT', color: C.gray },
  log: { glyph: 'LOG', color: C.gray },

  // Shell / build
  sh: { glyph: '$', color: C.shell },
  bash: { glyph: '$', color: C.shell },
  zsh: { glyph: '$', color: C.shell },
  fish: { glyph: '$', color: C.shell },
  ps1: { glyph: '>_', color: C.go },
  mk: { glyph: 'MK', color: C.gray },

  // Media / binary containers
  png: { glyph: 'IMG', color: C.image },
  jpg: { glyph: 'IMG', color: C.image },
  jpeg: { glyph: 'IMG', color: C.image },
  gif: { glyph: 'IMG', color: C.image },
  webp: { glyph: 'IMG', color: C.image },
  ico: { glyph: 'ICO', color: C.image },
  bmp: { glyph: 'IMG', color: C.image },
  avif: { glyph: 'IMG', color: C.image },
  svg: { glyph: 'SVG', color: C.image },
  woff: { glyph: 'FNT', color: C.font },
  woff2: { glyph: 'FNT', color: C.font },
  ttf: { glyph: 'FNT', color: C.font },
  otf: { glyph: 'FNT', color: C.font },
  eot: { glyph: 'FNT', color: C.font },
  zip: { glyph: 'ZIP', color: C.archive },
  tar: { glyph: 'ARC', color: C.archive },
  gz: { glyph: 'ARC', color: C.archive },
  xz: { glyph: 'ARC', color: C.archive },
  bz2: { glyph: 'ARC', color: C.archive },
  zst: { glyph: 'ARC', color: C.archive },
  '7z': { glyph: 'ARC', color: C.archive },
  rar: { glyph: 'ARC', color: C.archive },
  pdf: { glyph: 'PDF', color: C.pdf },
  mp4: { glyph: 'VID', color: C.video },
  mkv: { glyph: 'VID', color: C.video },
  mov: { glyph: 'VID', color: C.video },
  webm: { glyph: 'VID', color: C.video },
};

/** Whole-name overrides beat extension matching (Dockerfile, Cargo.toml…). */
const NAME_MAP: Record<string, FileIconGlyph> = {
  dockerfile: { glyph: 'DK', color: C.docker },
  '.dockerignore': { glyph: 'DK', color: C.docker },
  makefile: { glyph: 'MK', color: C.gray },
  'cargo.toml': { glyph: 'RS', color: C.rust },
  'cargo.lock': { glyph: 'LCK', color: C.gray },
  'go.mod': { glyph: 'GO', color: C.go },
  'go.sum': { glyph: 'GO', color: C.go },
  'requirements.txt': { glyph: 'PY', color: C.python },
  'pyproject.toml': { glyph: 'PY', color: C.python },
  gemfile: { glyph: 'RB', color: C.ruby },
  'cmakelists.txt': { glyph: 'CM', color: C.gray },
  license: { glyph: '©', color: C.gray },
  licence: { glyph: '©', color: C.gray },
  copying: { glyph: '©', color: C.gray },
  '.gitignore': { glyph: 'GIT', color: C.git },
  '.gitattributes': { glyph: 'GIT', color: C.git },
  '.gitmodules': { glyph: 'GIT', color: C.git },
  '.gitconfig': { glyph: 'GIT', color: C.git },
  '.gitkeep': { glyph: 'GIT', color: C.git },
  '.env': { glyph: 'ENV', color: C.env },
  readme: { glyph: 'MD', color: C.teal },
};

/** Extension → glyph fallback for anything unmapped: ≤3 chars of the
 * extension (gray); extension-less files get a text-lines glyph. */
const FALLBACK = (name: string): FileIconGlyph => {
  const ext = name.includes('.') ? name.split('.').pop()! : '';
  if (!ext) return { glyph: '≡', color: C.gray };
  return { glyph: ext.slice(0, 3).toUpperCase(), color: C.gray };
};

/** Map a file name to its chip glyph + color. Pure — no theme, no DOM. */
export function fileIcon(name: string): FileIconGlyph {
  const lower = name.toLowerCase();
  // Prefix families before exact/ext matching: .env.* and .git* variants.
  if (lower.startsWith('.env')) return { glyph: 'ENV', color: C.env };
  if (lower.startsWith('.git')) return { glyph: 'GIT', color: C.git };
  return NAME_MAP[lower] ?? EXT_MAP[lower.split('.').pop() ?? ''] ?? FALLBACK(name);
}
