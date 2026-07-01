//! Driver policy + resilience: the inline-viewport terminal options, the
//! plain-language error line, and graceful degradation to a non-TTY path.
//!
//! Scrollback safety is a construction rule, not a hope: the terminal runs an
//! **inline viewport of fixed height** — no alternate screen (`?1049`), no
//! whole-screen clear — so everything above the live band stays in the
//! terminal's native scrollback. Failures render as one plain warning line,
//! never a stack trace.

use ratatui::{TerminalOptions, Viewport};

/// The fixed height of the live region. Content above it is pushed into
/// native scrollback (insert-before), never cleared.
pub const LIVE_REGION_HEIGHT: u16 = 16;

/// The terminal options every driver uses: an inline viewport of fixed
/// height. This is the whole scrollback-safety contract — an alternate screen
/// or a full clear cannot happen through these options.
#[must_use]
pub fn terminal_options() -> TerminalOptions {
    TerminalOptions {
        viewport: Viewport::Inline(LIVE_REGION_HEIGHT),
    }
}

/// Render a failure as one plain warning line for a non-developer: first line
/// only, no panic/backtrace vocabulary, bounded length.
#[must_use]
pub fn plain_warning(context: &str, error: &str) -> String {
    let first_line = error.lines().next().unwrap_or("").trim();
    let cleaned = first_line
        .trim_start_matches("Error: ")
        .trim_start_matches("error: ");
    let mut message = format!("⚠  {context}: {cleaned}");
    if message.chars().count() > 200 {
        message = message.chars().take(200).collect::<String>() + "…";
    }
    message
}

/// Whether to degrade to the plain (line-based) path: any non-TTY stdout, or
/// an explicit plain request.
#[must_use]
pub fn should_degrade(stdout_is_tty: bool, plain_requested: bool) -> bool {
    plain_requested || !stdout_is_tty
}

/// The plain (non-TTY) rendering of a choice list: numbered rows, one per
/// line — pipeable, screen-reader-friendly, no cursor addressing at all.
#[must_use]
pub fn plain_menu(title: &str, rows: &[String]) -> String {
    let mut out = format!("{title}\n");
    for (i, row) in rows.iter().enumerate() {
        out.push_str(&format!("  {}. {row}\n", i + 1));
    }
    out
}

/// Best-effort UTF-8 console output on Windows, so box-drawing renders
/// without the user editing a shell profile. A failure is ignored — worst
/// case the borders look rough, which must never block a launch.
pub fn ensure_utf8_output() {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "chcp 65001 >NUL"])
            .status();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_viewport_is_inline_never_the_alternate_screen() {
        // Viewport::Inline is the scrollback-safe mode: ratatui's inline
        // viewport never emits the alt-screen switch (?1049) or a full clear.
        let options = terminal_options();
        assert!(
            matches!(options.viewport, Viewport::Inline(LIVE_REGION_HEIGHT)),
            "the driver must run an inline fixed-height viewport"
        );
    }

    #[test]
    fn failures_render_as_one_plain_line() {
        let warning = plain_warning(
            "Launch failed",
            "Error: GGUF not downloaded: D:/gguf/x.gguf (install the model first)\n\
             stack backtrace:\n   0: core::panicking",
        );
        assert_eq!(
            warning,
            "⚠  Launch failed: GGUF not downloaded: D:/gguf/x.gguf (install the model first)"
        );
        assert!(!warning.contains("backtrace"));
        assert!(!warning.contains('\n'));
        // Long messages are bounded.
        let long = plain_warning("x", &"words ".repeat(100));
        assert!(long.chars().count() <= 201);
    }

    #[test]
    fn non_tty_and_explicit_plain_both_degrade() {
        assert!(should_degrade(false, false), "a pipe degrades");
        assert!(should_degrade(true, true), "an explicit request degrades");
        assert!(
            !should_degrade(true, false),
            "an interactive TTY stays rich"
        );
    }

    #[test]
    fn the_plain_menu_is_numbered_lines_with_no_cursor_addressing() {
        let menu = plain_menu(
            "Ready to launch q36apex?",
            &[
                "Launch now (recommended settings)".to_string(),
                "Customize settings".to_string(),
            ],
        );
        assert_eq!(
            menu,
            "Ready to launch q36apex?\n  1. Launch now (recommended settings)\n  2. Customize settings\n"
        );
        assert!(!menu.contains('\u{1b}'), "no escape sequences at all");
    }
}
