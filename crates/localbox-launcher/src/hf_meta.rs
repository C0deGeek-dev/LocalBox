//! Hugging Face repo references and GGUF file listing over the Hub API.
//!
//! This is the read half of installing a model straight from a Hugging Face
//! repository: turn a copy-pasteable reference (`owner/repo` or a
//! `https://huggingface.co/owner/repo` URL) into a canonical repo id, then ask
//! the Hub's model-info API which GGUF files the repo holds and how big each is.
//! It performs no download and touches no disk — [`fetch`](crate::fetch) still
//! owns the transfer, and the catalog still owns persistence — so the listing
//! is a pure, side-effect-free step that the download command composes.

use serde::Deserialize;

/// A parsed Hugging Face repo reference, canonicalised to `owner` + `repo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HfRef {
    owner: String,
    repo: String,
}

impl HfRef {
    /// The canonical `owner/repo` repository id.
    #[must_use]
    pub fn repo_id(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    /// The owner (namespace) segment.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The repository-name segment.
    #[must_use]
    pub fn repo(&self) -> &str {
        &self.repo
    }
}

/// Why a string is not a usable Hugging Face repo reference.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HfRefError {
    /// The reference is empty or names no `owner/repo` pair.
    #[error("'{0}' is not a Hugging Face repo id (expected owner/repo or a https://huggingface.co/owner/repo URL)")]
    Malformed(String),
    /// A URL pointed at a host other than Hugging Face.
    #[error("'{0}' is not a huggingface.co URL")]
    WrongHost(String),
}

/// A GGUF file in a Hugging Face repo: its repo-relative name and, when the Hub
/// reported it (`?blobs=true`), its byte size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HfGgufFile {
    pub rfilename: String,
    pub size: Option<u64>,
}

/// The Hub metadata LocalBox carries into catalog synthesis. Descriptions are
/// already bounded plain text at this boundary; callers never receive remote
/// HTML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HfRepoInfo {
    pub files: Vec<HfGgufFile>,
    pub description: Option<String>,
}

/// Maximum number of Unicode scalar values retained from remote descriptive
/// metadata, including the trailing ellipsis when truncation is required.
pub const MAX_DESCRIPTION_CHARS: usize = 600;

fn decoded_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" | "#39" => Some('\''),
        "nbsp" => Some(' '),
        "ndash" => Some('–'),
        "mdash" => Some('—'),
        "hellip" => Some('…'),
        numeric if numeric.starts_with("#x") || numeric.starts_with("#X") => {
            u32::from_str_radix(&numeric[2..], 16)
                .ok()
                .and_then(char::from_u32)
        }
        numeric if numeric.starts_with('#') => {
            numeric[1..].parse::<u32>().ok().and_then(char::from_u32)
        }
        _ => None,
    }
}

fn decode_html_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative) = input[cursor..].find('&') {
        let start = cursor + relative;
        out.push_str(&input[cursor..start]);
        let tail = &input[start + 1..];
        let decoded = tail
            .find(';')
            .filter(|end| *end <= 16)
            .and_then(|end| decoded_entity(&tail[..end]).map(|ch| (end, ch)));
        if let Some((end, ch)) = decoded {
            out.push(ch);
            cursor = start + end + 2;
        } else {
            out.push('&');
            cursor = start + 1;
        }
    }
    out.push_str(&input[cursor..]);
    out
}

fn html_tag_name(after_open: &str) -> Option<(bool, &str)> {
    let trimmed = after_open.trim_start();
    let (closing, trimmed) = trimmed
        .strip_prefix('/')
        .map_or((false, trimmed), |rest| (true, rest.trim_start()));
    let end = trimmed
        .find(|ch: char| !ch.is_ascii_alphanumeric())
        .unwrap_or(trimmed.len());
    let name = &trimmed[..end];
    (!name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        .then_some((closing, name))
}

fn strip_html(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative) = input[cursor..].find('<') {
        let start = cursor + relative;
        out.push_str(&input[cursor..start]);
        if lower[start..].starts_with("<!--") {
            let Some(end) = lower[start + 4..].find("-->") else {
                cursor = input.len();
                break;
            };
            out.push(' ');
            cursor = start + 4 + end + 3;
            continue;
        }
        let after_open = &input[start + 1..];
        let declaration = after_open.trim_start().starts_with('!');
        let tag = html_tag_name(after_open);
        if tag.is_none() && !declaration {
            out.push('<');
            cursor = start + 1;
            continue;
        }
        let Some(open_end) = input[start + 1..].find('>') else {
            // A tag-shaped unterminated tail is markup, not useful prose.
            out.push(' ');
            cursor = input.len();
            break;
        };
        let after_tag = start + 1 + open_end + 1;
        if let Some((false, name)) = tag {
            if name.eq_ignore_ascii_case("script") || name.eq_ignore_ascii_case("style") {
                let closing = format!("</{}", name.to_ascii_lowercase());
                let Some(relative_close) = lower[after_tag..].find(&closing) else {
                    out.push(' ');
                    cursor = input.len();
                    break;
                };
                let close_start = after_tag + relative_close;
                let Some(close_end) = input[close_start..].find('>') else {
                    out.push(' ');
                    cursor = input.len();
                    break;
                };
                out.push(' ');
                cursor = close_start + close_end + 1;
                continue;
            }
        }
        out.push(' ');
        cursor = after_tag;
    }
    out.push_str(&input[cursor..]);
    out
}

/// Convert untrusted Hub descriptive metadata to readable bounded plain text.
/// Entities are decoded before tag stripping so encoded tags cannot cross the
/// boundary; script/style bodies and comments are removed with their content.
#[must_use]
pub fn plain_text_description(input: &str) -> Option<String> {
    let decoded = decode_html_entities(input);
    let without_html = strip_html(&decoded);
    let normalized = without_html
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.is_empty() {
        return None;
    }
    if normalized.chars().count() <= MAX_DESCRIPTION_CHARS {
        return Some(normalized);
    }
    let mut bounded = normalized
        .chars()
        .take(MAX_DESCRIPTION_CHARS.saturating_sub(1))
        .collect::<String>();
    bounded = bounded.trim_end().to_string();
    bounded.push('…');
    Some(bounded)
}

/// A failure listing a repo's files over the Hub API.
#[derive(Debug, thiserror::Error)]
pub enum HfMetaError {
    /// The Hub answered `404` — no such repo.
    #[error("Hugging Face repo '{0}' was not found")]
    NotFound(String),
    /// The Hub answered `401`/`403` — the repo is gated or private.
    #[error(
        "Hugging Face repo '{0}' is gated or private and needs an access token, \
         which LocalBox does not support yet"
    )]
    Gated(String),
    /// The repo exists but holds no `.gguf` files LocalBox can run.
    #[error("Hugging Face repo '{0}' has no .gguf files to download")]
    NotGguf(String),
    /// The Hub rate-limited the request (`429`).
    #[error("Hugging Face rate-limited the request; wait a moment and retry")]
    RateLimited,
    /// The request failed, or the Hub answered outside the contract.
    #[error("could not reach the Hugging Face API: {0}")]
    Network(String),
}

/// Parse a copy-pasteable reference into a canonical [`HfRef`].
///
/// Accepts a bare `owner/repo` and a `https://huggingface.co/owner/repo` URL
/// (with or without a trailing `/tree/<rev>`, `/blob/...`, `/resolve/...`,
/// query, or fragment). Backslashes normalise to `/`. A URL for any other host,
/// an empty segment, or a path-traversal segment (`.` / `..`) is rejected — the
/// caller then treats the argument as an unknown catalog name, not a repo.
///
/// # Errors
/// [`HfRefError::WrongHost`] for a non-Hugging-Face URL, [`HfRefError::Malformed`]
/// for anything that is not an `owner/repo` pair.
pub fn parse_hf_ref(input: &str) -> Result<HfRef, HfRefError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(HfRefError::Malformed(input.to_string()));
    }
    let normalized = trimmed.replace('\\', "/");

    // A URL (or scheme-relative / host-prefixed spelling) must name the Hub host.
    let path = if let Some(rest) = strip_scheme(&normalized) {
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        if !is_hf_host(host) {
            return Err(HfRefError::WrongHost(input.to_string()));
        }
        path
    } else if let Some(rest) = normalized.strip_prefix("huggingface.co/") {
        rest
    } else {
        normalized.as_str()
    };

    // Owner/repo are the first two non-empty path segments; anything after
    // (`/tree/main`, `/blob/...`) is ignored. Query/fragment are dropped first.
    let path = path
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_matches('/');
    let mut segments = path.split('/').filter(|s| !s.is_empty());
    let (Some(owner), Some(repo)) = (segments.next(), segments.next()) else {
        return Err(HfRefError::Malformed(input.to_string()));
    };
    if !is_safe_segment(owner) || !is_safe_segment(repo) {
        return Err(HfRefError::Malformed(input.to_string()));
    }
    Ok(HfRef {
        owner: owner.to_string(),
        repo: repo.to_string(),
    })
}

/// Strip a `scheme://` (or scheme-relative `//`) prefix, returning the
/// host-and-path remainder when the input was a URL.
fn strip_scheme(input: &str) -> Option<&str> {
    if let Some((_scheme, rest)) = input.split_once("://") {
        Some(rest)
    } else {
        input.strip_prefix("//")
    }
}

fn is_hf_host(host: &str) -> bool {
    let host = host.split(['?', '#']).next().unwrap_or(host);
    host.eq_ignore_ascii_case("huggingface.co") || host.eq_ignore_ascii_case("www.huggingface.co")
}

/// A path segment is a safe owner/repo name: non-empty, not a traversal dot run,
/// and free of characters a repo id never contains.
fn is_safe_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment.contains("..")
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// The Hugging Face base URL. Defaults to the real Hub; `LOCALBOX_HF_ENDPOINT`
/// overrides it (with no trailing slash) so an end-to-end test can point the
/// listing and the download at a loopback server.
#[must_use]
pub fn endpoint() -> String {
    std::env::var("LOCALBOX_HF_ENDPOINT")
        .ok()
        .map(|base| base.trim_end_matches('/').to_string())
        .filter(|base| !base.is_empty())
        .unwrap_or_else(|| "https://huggingface.co".to_string())
}

/// Percent-encode a repo-relative path (each segment), normalising backslashes.
fn encode_path(path: &str) -> String {
    path.replace('\\', "/")
        .split('/')
        .map(crate::fetch::escape_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// The Hub model-info URL for a repo id, requesting per-file blob sizes.
#[must_use]
pub fn model_info_url(repo_id: &str) -> String {
    format!(
        "{}/api/models/{}?blobs=true",
        endpoint(),
        encode_path(repo_id)
    )
}

/// The shape LocalBox reads from the Hub model-info response. Extra fields are
/// ignored; `size` is present only when the request asked for blobs.
#[derive(Debug, Deserialize)]
struct ModelInfo {
    #[serde(default)]
    siblings: Vec<Sibling>,
    #[serde(default)]
    description: Option<serde_json::Value>,
    #[serde(rename = "cardData", default)]
    card_data: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct Sibling {
    rfilename: String,
    #[serde(default)]
    size: Option<u64>,
}

/// The GGUF-only subset of a sibling list, preserving Hub order.
#[must_use]
fn gguf_files(siblings: Vec<Sibling>) -> Vec<HfGgufFile> {
    siblings
        .into_iter()
        .filter(|s| s.rfilename.to_ascii_lowercase().ends_with(".gguf"))
        .map(|s| HfGgufFile {
            rfilename: s.rfilename,
            size: s.size,
        })
        .collect()
}

/// Map a non-success Hub status to the typed error it means; `None` for `200`.
#[must_use]
fn meta_error_for_status(status: u16, repo_id: &str) -> Option<HfMetaError> {
    match status {
        200 => None,
        404 => Some(HfMetaError::NotFound(repo_id.to_string())),
        401 | 403 => Some(HfMetaError::Gated(repo_id.to_string())),
        429 => Some(HfMetaError::RateLimited),
        other => Some(HfMetaError::Network(format!(
            "unexpected HTTP status {other}"
        ))),
    }
}

/// List the GGUF files in a Hugging Face repo over the Hub model-info API.
///
/// # Errors
/// [`HfMetaError`] on a not-found / gated / rate-limited / unreachable repo, or
/// when the repo holds no `.gguf` files.
pub async fn list_gguf_files(
    client: &reqwest::Client,
    hf: &HfRef,
) -> Result<Vec<HfGgufFile>, HfMetaError> {
    inspect_hf_repo(client, hf).await.map(|info| info.files)
}

/// Read the GGUF listing and any description supplied by the Hub model-info
/// response. The description is sanitized before it leaves this module.
///
/// # Errors
/// The same typed Hub failures as [`list_gguf_files`].
pub async fn inspect_hf_repo(
    client: &reqwest::Client,
    hf: &HfRef,
) -> Result<HfRepoInfo, HfMetaError> {
    fetch_repo_info(client, &model_info_url(&hf.repo_id()), &hf.repo_id()).await
}

/// GGUF-only adapter retained for the loopback status/listing tests.
#[cfg(test)]
async fn fetch_gguf_files(
    client: &reqwest::Client,
    url: &str,
    repo_id: &str,
) -> Result<Vec<HfGgufFile>, HfMetaError> {
    fetch_repo_info(client, url, repo_id)
        .await
        .map(|info| info.files)
}

/// The metadata driver, parameterised on the URL so tests can point it at a
/// loopback server. Mirrors [`crate::fetch::download_with_resume`]'s split of a
/// pure classifier (`meta_error_for_status`) from the one async call.
async fn fetch_repo_info(
    client: &reqwest::Client,
    url: &str,
    repo_id: &str,
) -> Result<HfRepoInfo, HfMetaError> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| HfMetaError::Network(e.to_string()))?;
    if let Some(err) = meta_error_for_status(response.status().as_u16(), repo_id) {
        return Err(err);
    }
    let info: ModelInfo = response
        .json()
        .await
        .map_err(|e| HfMetaError::Network(e.to_string()))?;
    let description = info
        .description
        .as_ref()
        .and_then(serde_json::Value::as_str)
        .or_else(|| info.card_data.as_ref()?.get("description")?.as_str())
        .and_then(plain_text_description);
    let files = gguf_files(info.siblings);
    if files.is_empty() {
        return Err(HfMetaError::NotGguf(repo_id.to_string()));
    }
    Ok(HfRepoInfo { files, description })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_owner_repo_parses() {
        let hf = parse_hf_ref("mradermacher/Some-Model-i1-GGUF").unwrap();
        assert_eq!(hf.owner(), "mradermacher");
        assert_eq!(hf.repo(), "Some-Model-i1-GGUF");
        assert_eq!(hf.repo_id(), "mradermacher/Some-Model-i1-GGUF");
    }

    #[test]
    fn a_full_url_parses_and_ignores_trailing_path_query_and_fragment() {
        for input in [
            "https://huggingface.co/bartowski/Llama-3.2-1B-Instruct-GGUF",
            "https://huggingface.co/bartowski/Llama-3.2-1B-Instruct-GGUF/tree/main",
            "https://www.huggingface.co/bartowski/Llama-3.2-1B-Instruct-GGUF?foo=1#frag",
            "http://huggingface.co/bartowski/Llama-3.2-1B-Instruct-GGUF/blob/main/x.gguf",
            "huggingface.co/bartowski/Llama-3.2-1B-Instruct-GGUF",
            // Backslash spellings a shell or paste can introduce.
            "https://huggingface.co\\bartowski\\Llama-3.2-1B-Instruct-GGUF",
        ] {
            let hf = parse_hf_ref(input).unwrap_or_else(|e| panic!("{input}: {e}"));
            assert_eq!(
                hf.repo_id(),
                "bartowski/Llama-3.2-1B-Instruct-GGUF",
                "{input}"
            );
        }
    }

    #[test]
    fn a_non_hub_url_is_rejected_as_wrong_host() {
        assert_eq!(
            parse_hf_ref("https://example.com/owner/repo"),
            Err(HfRefError::WrongHost(
                "https://example.com/owner/repo".to_string()
            ))
        );
    }

    #[test]
    fn malformed_and_hostile_inputs_are_rejected() {
        for bad in [
            "",
            "   ",
            "onlyowner",
            "owner/",
            "/repo",
            "../etc/passwd",
            "owner/..",
            "owner/../repo",
            "https://huggingface.co/owner",
        ] {
            assert!(
                matches!(parse_hf_ref(bad), Err(HfRefError::Malformed(_))),
                "expected {bad:?} to be malformed, got {:?}",
                parse_hf_ref(bad)
            );
        }
        // An interior empty segment (a paste artifact) is tolerated: the first
        // two non-empty segments win, the same rule that lets a URL path carry a
        // trailing `/tree/main`.
        assert_eq!(
            parse_hf_ref("owner//repo").map(|h| h.repo_id()),
            Ok("owner/repo".to_string())
        );
    }

    #[test]
    fn the_model_info_url_escapes_segments_and_requests_blobs() {
        assert_eq!(
            model_info_url("owner/model-GGUF"),
            "https://huggingface.co/api/models/owner/model-GGUF?blobs=true"
        );
    }

    #[test]
    fn only_gguf_siblings_survive_and_sizes_pass_through() {
        let siblings = vec![
            Sibling {
                rfilename: "README.md".to_string(),
                size: Some(10),
            },
            Sibling {
                rfilename: "model.Q4_K_M.gguf".to_string(),
                size: Some(4_000),
            },
            Sibling {
                rfilename: "MODEL.IQ4_XS.GGUF".to_string(),
                size: None,
            },
            Sibling {
                rfilename: "config.json".to_string(),
                size: Some(1),
            },
        ];
        let files = gguf_files(siblings);
        assert_eq!(
            files,
            vec![
                HfGgufFile {
                    rfilename: "model.Q4_K_M.gguf".to_string(),
                    size: Some(4_000)
                },
                HfGgufFile {
                    rfilename: "MODEL.IQ4_XS.GGUF".to_string(),
                    size: None
                },
            ]
        );
    }

    #[test]
    fn statuses_map_to_typed_errors() {
        assert!(meta_error_for_status(200, "o/r").is_none());
        assert!(matches!(
            meta_error_for_status(404, "o/r"),
            Some(HfMetaError::NotFound(_))
        ));
        assert!(matches!(
            meta_error_for_status(401, "o/r"),
            Some(HfMetaError::Gated(_))
        ));
        assert!(matches!(
            meta_error_for_status(403, "o/r"),
            Some(HfMetaError::Gated(_))
        ));
        assert!(matches!(
            meta_error_for_status(429, "o/r"),
            Some(HfMetaError::RateLimited)
        ));
        assert!(matches!(
            meta_error_for_status(500, "o/r"),
            Some(HfMetaError::Network(_))
        ));
    }

    #[test]
    fn descriptions_become_bounded_readable_plain_text() {
        let cases = [
            (
                "<div>Hello <b>world &amp; friends</b><br>Next</div>",
                "Hello world & friends Next",
            ),
            (
                "Useful<script>alert('<b>bad</b>')</script><style>b{color:red}</style> text",
                "Useful text",
            ),
            (
                "&lt;strong&gt;Nested &#x1F4A1; &#8212; safe&lt;/strong&gt;",
                "Nested 💡 — safe",
            ),
            (
                "Already plain — keep me exactly.",
                "Already plain — keep me exactly.",
            ),
            ("Useful prefix <b unterminated", "Useful prefix"),
            ("before<!-- hidden <i>tag</i> -->after", "before after"),
        ];
        for (input, expected) in cases {
            assert_eq!(plain_text_description(input).as_deref(), Some(expected));
        }
        assert_eq!(plain_text_description(" <script>only bad</script> "), None);

        let long = "word ".repeat(300);
        let bounded = plain_text_description(&long).unwrap();
        assert!(bounded.chars().count() <= MAX_DESCRIPTION_CHARS);
        assert!(bounded.ends_with('…'));
    }

    /// A one-shot HTTP/1.1 responder on a loopback port (same shape as the
    /// downloader's test harness): answers the first request with `status_line`
    /// and `body`, then exits.
    fn serve_once(status_line: &'static str, body: &'static str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).unwrap_or(0);
            let response = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        format!("http://{addr}/api/models/o/r")
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn a_successful_listing_returns_gguf_files() {
        let body = r#"{"siblings":[
            {"rfilename":"README.md","size":10},
            {"rfilename":"m.Q4_K_M.gguf","size":4000},
            {"rfilename":"m.IQ4_XS.gguf","size":3000}
        ]}"#;
        let url = serve_once("200 OK", body);
        let client = reqwest::Client::new();
        let files = block_on(fetch_gguf_files(&client, &url, "o/r")).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].rfilename, "m.Q4_K_M.gguf");
        assert_eq!(files[0].size, Some(4000));
    }

    #[test]
    fn model_info_sanitizes_description_before_returning_it() {
        let body = r#"{
            "description":"<p>Useful &amp; safe</p><script>bad()</script>",
            "siblings":[{"rfilename":"m.gguf","size":4000}]
        }"#;
        let url = serve_once("200 OK", body);
        let client = reqwest::Client::new();
        let info = block_on(fetch_repo_info(&client, &url, "o/r")).unwrap();
        assert_eq!(info.description.as_deref(), Some("Useful & safe"));
        assert_eq!(info.files.len(), 1);
    }

    #[test]
    fn a_repo_without_gguf_files_is_an_error() {
        let url = serve_once("200 OK", r#"{"siblings":[{"rfilename":"README.md"}]}"#);
        let client = reqwest::Client::new();
        assert!(matches!(
            block_on(fetch_gguf_files(&client, &url, "o/r")),
            Err(HfMetaError::NotGguf(_))
        ));
    }

    #[test]
    fn a_not_found_status_is_typed() {
        let url = serve_once("404 Not Found", "{}");
        let client = reqwest::Client::new();
        assert!(matches!(
            block_on(fetch_gguf_files(&client, &url, "o/r")),
            Err(HfMetaError::NotFound(_))
        ));
    }

    #[test]
    fn a_gated_status_is_typed() {
        let url = serve_once("403 Forbidden", "{}");
        let client = reqwest::Client::new();
        assert!(matches!(
            block_on(fetch_gguf_files(&client, &url, "o/r")),
            Err(HfMetaError::Gated(_))
        ));
    }
}
