/**
 * clipboard.ts — copy-to-clipboard with a non-secure-context fallback.
 *
 * `navigator.clipboard` exists ONLY in secure contexts (HTTPS or
 * localhost) — the server is routinely reached over plain HTTP from a LAN
 * address, where it is undefined and every copy would silently fail. The
 * fallback is the classic hidden-textarea + execCommand('copy') path:
 * deprecated, but the only universal one.
 *
 * Provides: copyText
 */

export async function copyText(text: string): Promise<boolean> {
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // Permission denied / document not focused — fall through to the
      // legacy path instead of failing silently.
    }
  }
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.setAttribute('readonly', '');
  // Fixed + invisible: offscreen positioning scrolls some browsers.
  ta.style.position = 'fixed';
  ta.style.opacity = '0';
  ta.style.pointerEvents = 'none';
  document.body.appendChild(ta);
  ta.select();
  ta.setSelectionRange(0, text.length);
  let ok = false;
  try {
    ok = document.execCommand('copy');
  } catch {
    ok = false;
  }
  ta.remove();
  return ok;
}
