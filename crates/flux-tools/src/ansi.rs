//! ANSI escape-sequence stripping for captured shell output.
//!
//! SCOPE: only the bash tool strips (see `shell.rs` BashTool::execute).
//! Deliberately NOT applied elsewhere:
//! - file tools (read_file / grep / glob) must stay byte-faithful — the
//!   model edits from their content, and a strip would silently desync it
//!   from the file (offset/char-count mismatch on the next edit);
//! - MCP results are protocol text and stay untouched.
//!
//! Bash output is terminal chatter — color there is noise for both the
//! model and the UI card.
//!
//! Piping (the runner's `Stdio::piped`) already suppresses color for
//! isatty-aware programs; this removes the residue from programs that emit
//! unconditionally (e.g. `tracing` subscribers with the ansi feature).
//!
//! Recognized and removed:
//! - CSI sequences `ESC [ params intermediates final` (SGR colors, cursor
//!   movement, erase, …);
//! - OSC / DCS / SOS / PM / APC string sequences, terminated by BEL or ST
//!   (`ESC \`) — window titles, hyperlinks, …;
//! - two- and three-character escapes (`ESC (B` charset, `ESC 7`/`ESC M`,
//!   …) including intermediate bytes;
//! - C0 controls except `\n` and `\t` (so bare `\r` goes too — progress-bar
//!   overwrite frames collapse onto one line; the accepted tradeoff, same
//!   as mainstream coding agents) and DEL.
//!
//! Malformed input degrades safely: a sequence that never terminates within
//! its grammar aborts at the first non-conforming char and that char is
//! reprocessed as normal text (content is preserved, at worst one stray ESC
//! is dropped). C1 (U+009B as a char) is honored as a CSI introducer in the
//! slow path; the zero-copy fast path does not scan for it (a raw U+009B in
//! tool output without any ESC or C0 byte is pathological).
//!
//! Provides: strip_ansi

use std::borrow::Cow;

/** Emit `c` as ground text: keep `\n` and `\t`, drop every other control. */
fn ground_push(out: &mut String, c: char) {
    match c {
        '\n' | '\t' => out.push(c),
        c if (c as u32) < 0x20 || c == '\u{7f}' => {}
        c => out.push(c),
    }
}

/** Ground-state transition for one char — control dispatch plus text.
 * Resets the state to Ground (abort paths reuse it to reprocess a char). */
fn ground_step(st: &mut State, out: &mut String, c: char) {
    match c {
        '\x1b' => *st = State::Esc,
        '\u{9b}' => *st = State::Csi,
        c => {
            *st = State::Ground;
            ground_push(out, c);
        }
    }
}

enum State {
    /// Normal text.
    Ground,
    /// Just consumed ESC.
    Esc,
    /// Inside `ESC [ …` — params/intermediates until a final byte.
    Csi,
    /// Inside an unterminated string sequence (OSC/DCS/SOS/PM/APC) —
    /// payload until BEL or ST.
    Str,
}

/// Strip ANSI escape sequences and control noise from captured terminal
/// output. Escape-free input returns borrowed (the common path — most
/// captured output is already colorless).
pub(crate) fn strip_ansi(input: &str) -> Cow<'_, str> {
    // ESC (0x1B) can never appear inside a multi-byte UTF-8 sequence, so
    // the byte scan is exact; C0/DEL bytes are single-byte by construction.
    let dirty = input
        .bytes()
        .any(|b| b == 0x1b || b == 0x7f || (b < 0x20 && b != b'\n' && b != b'\t'));
    if !dirty {
        return Cow::Borrowed(input);
    }

    let mut out = String::with_capacity(input.len());
    let mut st = State::Ground;
    let mut iter = input.chars().peekable();
    while let Some(c) = iter.next() {
        match st {
            State::Ground => ground_step(&mut st, &mut out, c),
            State::Esc => match c {
                '[' => st = State::Csi,
                // String sequences: OSC, DCS, SOS, PM, APC.
                ']' | 'P' | 'X' | '^' | '_' => st = State::Str,
                // Intermediate bytes of a multi-char escape (`ESC ( B`).
                '\u{20}'..='\u{2f}' => {}
                // Final byte of a two/three-char escape (`ESC 7`, `ESC M`).
                '\u{30}'..='\u{7e}' => st = State::Ground,
                other => ground_step(&mut st, &mut out, other),
            },
            State::Csi => match c {
                // Params (0-9 : ; < = > ?) and intermediates — keep consuming.
                '\u{20}'..='\u{3f}' => {}
                // Final byte — sequence complete.
                '\u{40}'..='\u{7e}' => st = State::Ground,
                // Malformed (text or control char mid-sequence): abort the
                // sequence and reprocess the char as ground text.
                other => ground_step(&mut st, &mut out, other),
            },
            State::Str => match c {
                // Legacy BEL terminator.
                '\x07' => st = State::Ground,
                '\x1b' => match iter.peek() {
                    // ST (`ESC \`) — consume the backslash and close.
                    Some('\\') => {
                        iter.next();
                        st = State::Ground;
                    }
                    // A bare ESC aborts the string and starts a new escape;
                    // the ESC is consumed, the next char reprocesses as such.
                    _ => st = State::Esc,
                },
                _ => {} // payload — dropped
            },
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(s: &str) -> String {
        strip_ansi(s).into_owned()
    }

    #[test]
    fn clean_text_is_returned_borrowed_and_unchanged() {
        let s = "plain output 123 中文";
        assert!(matches!(strip_ansi(s), Cow::Borrowed(_)));
        assert_eq!(strip(s), s);
    }

    #[test]
    fn sgr_colors_are_stripped() {
        assert_eq!(strip("\x1b[32mgreen\x1b[0m"), "green");
        assert_eq!(strip("\x1b[38;2;12;34;56mtrue\x1b[0mX"), "trueX");
        assert_eq!(strip("\x1b[1;4;90mbold\x1b[m"), "bold");
    }

    #[test]
    fn tracing_log_line_is_cleaned() {
        // The motivating sample: a tracing_subscriber line with dim/green SGRs.
        let raw = "\x1b[2m2026-09-15T14:05:09.650455Z\x1b[0m \x1b[32m INFO\x1b[0m \x1b[2mflux_server\x1b[0m\x1b[2m:\x1b[0m opening store";
        assert_eq!(
            strip(raw),
            "2026-09-15T14:05:09.650455Z  INFO flux_server: opening store"
        );
    }

    #[test]
    fn cursor_and_erase_sequences_are_stripped() {
        assert_eq!(strip("\x1b[2J\x1b[Hcleared"), "cleared");
        assert_eq!(strip("\x1b[?25lhidden\x1b[?25h"), "hidden");
    }

    #[test]
    fn osc_sequences_end_at_bel_or_st() {
        assert_eq!(strip("\x1b]0;window title\x07rest"), "rest");
        // OSC 8 hyperlink terminated by ST
        assert_eq!(
            strip("\x1b]8;;http://example.com\x1b\\link\x1b]8;;\x1b\\"),
            "link"
        );
    }

    #[test]
    fn charset_and_two_char_escapes_are_stripped() {
        assert_eq!(strip("\x1b(Bhello"), "hello");
        assert_eq!(strip("a\x1b7b\x1b8c"), "abc");
    }

    #[test]
    fn newlines_and_tabs_survive() {
        assert_eq!(strip("a\r\nb\tc"), "a\nb\tc");
        // bare \r dropped (progress-bar overwrite frames collapse — accepted)
        assert_eq!(strip("10%\r20%\r30%\ndone"), "10%20%30%\ndone");
    }

    #[test]
    fn bel_and_del_noise_is_dropped() {
        assert_eq!(strip("a\x07b\x7fc"), "abc");
    }

    #[test]
    fn malformed_csi_aborts_and_preserves_text() {
        // A non-terminating CSI ends at the first non-conforming char.
        assert_eq!(strip("a\x1b[3\nb"), "a\nb");
        assert_eq!(strip("a\x1b[38;2中文"), "a中文");
    }

    #[test]
    fn truncated_escape_at_eof_is_dropped() {
        assert_eq!(strip("abc\x1b"), "abc");
        assert_eq!(strip("abc\x1b["), "abc");
        assert_eq!(strip("abc\x1b]0;unterminated"), "abc");
    }

    #[test]
    fn non_ascii_text_is_preserved() {
        assert_eq!(strip("héllo 中文 \x1b[31mred\x1b[0m ↗"), "héllo 中文 red ↗");
    }
}
