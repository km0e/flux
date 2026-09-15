# Bundled fonts

All families are SIL Open Font License 1.1 (OFL-1.1) — free to bundle,
embed and redistribute; see the OFL text at
https://github.com/JetBrains/JetBrainsMono/blob/master/OFL.txt and
https://github.com/ryanoasis/nerd-fonts (fonts are OFL-1.1; the repo's
MIT license covers the patch scripts only) and
https://github.com/IBM/plex/blob/master/LICENSE.txt.

Two voices carry the UI (see styles/fonts.css):

- **IBM Plex Sans** — the human voice: prose, UI chrome, headings.
  The complete woff2 face covers Latin/Greek/Cyrillic; CJK content
  falls through the stack to system fonts by design.
- **JetBrains Mono** — the machine voice: code, paths, tool args,
  token accounting, and the terminal. Mono is applied ONLY where the
  content itself is machine output, never as decoration.

- `IBMPlexSans-{Regular,Medium,SemiBold,Italic}.woff2` — IBM Plex Sans
  v1.1.0 (IBM, https://github.com/IBM/plex) — the UI face
- `JetBrainsMono-{Regular,Bold,Italic}.woff2` — JetBrains Mono v2.304
  (JetBrains, https://www.jetbrains.com/lp/mono/)
- `JetBrainsMonoNerdFontMono-Regular.woff2` — JetBrainsMono Nerd Font Mono
  v3.4.0 (ryanoasis/nerd-fonts — the patched variant that adds the icon /
  powerline glyph range; the "Mono" variant keeps icons single-width so
  terminal cells stay aligned). Upstream ships TTF only; this woff2 is
  converted locally by `scripts/fetch-fonts.sh` at refresh time.
