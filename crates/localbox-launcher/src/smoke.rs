//! The launch smoke test: prove the served model returns *visible, sane* text
//! before handing it an agent.
//!
//! A local stack can be up and healthy at the HTTP level while the model is
//! unusable — emitting nothing, a `[no output]` marker, a punctuation flood
//! (`////////…`, the classic GPU/driver mismatch signature), or a stuck token
//! loop. The smoke test asks one tiny question and evaluates the reply:
//! thinking output is stripped FIRST (a reasoning model may legitimately fill
//! the budget with `<think>` before a short answer), then the degenerate
//! detectors run on what a user would actually see.

use serde::Deserialize;

/// Whether visible text looks degenerate: the `[no output]` marker, an 8+ run
/// of flood punctuation, or any token repeated 10+ times consecutively.
#[must_use]
pub fn is_degenerate_text(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        // Emptiness is "no answer", not degeneracy — the caller distinguishes.
        return false;
    }
    if trimmed == "[no output]" {
        return true;
    }
    // A run of 8+ identical flood characters anywhere in the text.
    const FLOOD: &[char] = &['/', '\\', '#', '*', '=', '.', '~', '-'];
    let mut previous: Option<char> = None;
    let mut run = 0usize;
    for ch in trimmed.chars() {
        if FLOOD.contains(&ch) && previous == Some(ch) {
            run += 1;
            if run >= 8 {
                return true;
            }
        } else {
            run = 1;
        }
        previous = Some(ch);
    }
    // Any whitespace-split token repeated 10+ times consecutively.
    let mut previous_token: Option<&str> = None;
    let mut token_run = 0usize;
    for token in trimmed.split_whitespace() {
        if previous_token == Some(token) {
            token_run += 1;
        } else {
            previous_token = Some(token);
            token_run = 1;
        }
        if token_run >= 10 {
            return true;
        }
    }
    false
}

/// Strip `<think>…</think>` blocks (and an unterminated trailing `<think>`)
/// case-insensitively, returning what a user would actually see.
#[must_use]
pub fn strip_think(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let lower = text.to_lowercase();
    let mut cursor = 0usize;
    while let Some(start_rel) = lower[cursor..].find("<think>") {
        let start = cursor + start_rel;
        out.push_str(&text[cursor..start]);
        match lower[start..].find("</think>") {
            Some(end_rel) => {
                cursor = start + end_rel + "</think>".len();
            }
            None => {
                // Unterminated think block: everything after it is invisible.
                cursor = text.len();
                break;
            }
        }
    }
    out.push_str(&text[cursor..]);
    out.trim().to_string()
}

/// One content block of an Anthropic `/v1/messages` reply.
#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(default)]
    text: Option<String>,
}

/// The subset of an Anthropic reply the smoke test reads.
#[derive(Debug, Deserialize)]
struct MessagesReply {
    #[serde(default)]
    content: Vec<ContentBlock>,
}

/// The smoke verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmokeResult {
    /// The model answered with visible, non-degenerate text.
    pub ok: bool,
    /// The raw reply text (thinking included).
    pub text: String,
    /// What a user would see (thinking stripped).
    pub visible_text: String,
    /// The visible text tripped a degenerate detector.
    pub degenerate: bool,
    /// The failure reason; empty on success.
    pub error: String,
}

/// Evaluate a raw reply body (Anthropic `/v1/messages` JSON) into the smoke
/// verdict. The HTTP call is the caller's; this is the deterministic part.
#[must_use]
pub fn evaluate_smoke_reply(body: &str) -> SmokeResult {
    let text = serde_json::from_str::<MessagesReply>(body)
        .map(|reply| {
            reply
                .content
                .into_iter()
                .filter_map(|block| block.text)
                .filter(|t| !t.trim().is_empty())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
        .trim()
        .to_string();

    evaluate_smoke_text(&text)
}

/// Evaluate already-extracted reply text into the smoke verdict.
#[must_use]
pub fn evaluate_smoke_text(text: &str) -> SmokeResult {
    let visible = strip_think(text);
    let degenerate = is_degenerate_text(&visible);
    let answered = !visible.is_empty() && !degenerate;
    let error = if answered {
        String::new()
    } else if degenerate {
        "degenerate response text".to_string()
    } else if text.trim().is_empty() {
        "no response text".to_string()
    } else {
        "no visible response text after stripping thinking output".to_string()
    };
    SmokeResult {
        ok: answered,
        text: text.to_string(),
        visible_text: visible,
        degenerate,
        error,
    }
}

/// Render a failed smoke result for the operator: the error when present,
/// otherwise a whitespace-collapsed, bounded snippet of what came back.
#[must_use]
pub fn format_smoke_failure(smoke: &SmokeResult) -> String {
    if !smoke.error.is_empty() {
        return smoke.error.clone();
    }
    let snippet = if !smoke.visible_text.trim().is_empty() {
        &smoke.visible_text
    } else {
        &smoke.text
    };
    let collapsed = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return "no visible response text".to_string();
    }
    let bounded: String = if collapsed.chars().count() > 160 {
        let mut s: String = collapsed.chars().take(160).collect();
        s.push_str("...");
        s
    } else {
        collapsed
    };
    format!("unexpected smoke response: {bounded}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_flood_marker_and_repeat_detectors_fire() {
        // The GPU/driver-mismatch signature: a slash flood.
        assert!(is_degenerate_text("//////////"));
        assert!(is_degenerate_text("prefix ---------- suffix"));
        assert!(is_degenerate_text("[no output]"));
        // A stuck token loop.
        assert!(is_degenerate_text(&"same ".repeat(12)));
        // Healthy answers pass; 7-runs and short repeats stay under threshold.
        assert!(!is_degenerate_text("Yes, I'm working. How can I help?"));
        assert!(!is_degenerate_text("-------"));
        assert!(!is_degenerate_text(&"ok ".repeat(9)));
        assert!(!is_degenerate_text(""));
    }

    #[test]
    fn thinking_is_stripped_before_the_detectors_run() {
        // A reasoning model may fill its budget with think-noise before a fine
        // answer — that must NOT read as degenerate.
        let reply = "<think>////////// hmm ////////// </think>All good here.";
        let smoke = evaluate_smoke_text(reply);
        assert!(smoke.ok, "{smoke:?}");
        assert_eq!(smoke.visible_text, "All good here.");
        // Case-insensitive and unterminated blocks strip too.
        assert_eq!(strip_think("<THINK>x</THINK>visible"), "visible");
        assert_eq!(strip_think("visible<think>never closed"), "visible");
    }

    #[test]
    fn verdicts_name_their_failure_mode() {
        assert_eq!(evaluate_smoke_text("").error, "no response text");
        assert_eq!(
            evaluate_smoke_text("<think>only thinking</think>").error,
            "no visible response text after stripping thinking output"
        );
        let degenerate = evaluate_smoke_text("//////////");
        assert!(degenerate.degenerate);
        assert_eq!(degenerate.error, "degenerate response text");
    }

    #[test]
    fn the_anthropic_reply_body_parses_into_a_verdict() {
        let body = r#"{"content":[{"type":"text","text":"Working fine."}],"role":"assistant"}"#;
        let smoke = evaluate_smoke_reply(body);
        assert!(smoke.ok);
        assert_eq!(smoke.visible_text, "Working fine.");
        // Junk bodies fail closed as no-response, never panic.
        assert!(!evaluate_smoke_reply("not json").ok);
        assert!(!evaluate_smoke_reply(r#"{"content":[]}"#).ok);
    }

    #[test]
    fn failure_formatting_is_bounded_and_collapsed() {
        let mut smoke = evaluate_smoke_text("some    odd\n\nanswer");
        smoke.ok = false;
        smoke.error = String::new();
        assert_eq!(
            format_smoke_failure(&smoke),
            "unexpected smoke response: some odd answer"
        );
        let long = evaluate_smoke_text(&"word ".repeat(80));
        let mut long_failure = long.clone();
        long_failure.error = String::new();
        let rendered = format_smoke_failure(&long_failure);
        assert!(rendered.len() <= "unexpected smoke response: ".len() + 163);
        assert!(rendered.ends_with("..."));
        // With an explicit error, the error wins.
        let errored = evaluate_smoke_text("");
        assert_eq!(format_smoke_failure(&errored), "no response text");
    }
}
