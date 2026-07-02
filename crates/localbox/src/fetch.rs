//! Resumable HTTP download for model files.
//!
//! Mirrors the download contract users already rely on: a `.partial` sidecar
//! accumulates bytes, an interrupted pull resumes with a `Range` request, a
//! `416` answer means the partial is already the full file, and the final
//! name only ever appears fully written (rename-on-complete).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// A download failure.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The HTTP request failed or answered outside the resume contract.
    #[error("download failed: {0}")]
    Http(String),
    /// Local file I/O failed.
    #[error("download file error: {0}")]
    Io(String),
}

/// Percent-encode one path segment (RFC 3986 unreserved characters pass).
#[must_use]
pub fn escape_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The direct download URL for a file within a Hugging Face repo. Backslash
/// spellings normalize to `/`; each segment is percent-encoded.
#[must_use]
pub fn hf_download_url(repo: &str, file_name: &str) -> String {
    let path = file_name
        .replace('\\', "/")
        .split('/')
        .map(escape_segment)
        .collect::<Vec<_>>()
        .join("/");
    format!("https://huggingface.co/{repo}/resolve/main/{path}")
}

/// The sidecar path bytes accumulate in until the download completes.
#[must_use]
pub fn partial_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    dest.with_file_name(name)
}

/// The `Range` header value resuming after `existing_bytes`, when any.
#[must_use]
pub fn range_header(existing_bytes: u64) -> Option<String> {
    (existing_bytes > 0).then(|| format!("bytes={existing_bytes}-"))
}

/// How to treat the server's answer to a (possibly ranged) download request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeOutcome {
    /// `206`: the server honors the range — append to the partial.
    Append,
    /// `200`: the server ignored (or was not sent) a range — restart from zero.
    Restart,
    /// `416` on a resume: the partial already holds the full file.
    AlreadyComplete,
}

/// Classify a response status under the resume contract; `None` = failure.
#[must_use]
pub fn classify_range_status(status: u16, resume_requested: bool) -> Option<RangeOutcome> {
    match status {
        206 if resume_requested => Some(RangeOutcome::Append),
        200 => Some(RangeOutcome::Restart),
        416 if resume_requested => Some(RangeOutcome::AlreadyComplete),
        _ => None,
    }
}

/// Download `url` to `dest` with append-resume via the `.partial` sidecar.
/// The destination appears only after all bytes landed; an existing `dest`
/// short-circuits as already downloaded.
///
/// # Errors
/// [`FetchError`] on HTTP failure outside the resume contract or on file I/O.
pub async fn download_with_resume(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
) -> Result<(), FetchError> {
    if dest.is_file() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| FetchError::Io(e.to_string()))?;
    }
    let partial = partial_path(dest);
    let existing = fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);

    let mut request = client.get(url);
    if let Some(range) = range_header(existing) {
        request = request.header(reqwest::header::RANGE, range);
    }
    let mut response = request
        .send()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?;

    let status = response.status().as_u16();
    let outcome = classify_range_status(status, existing > 0)
        .ok_or_else(|| FetchError::Http(format!("{url}: unexpected HTTP status {status}")))?;

    match outcome {
        RangeOutcome::AlreadyComplete => {
            return fs::rename(&partial, dest).map_err(|e| FetchError::Io(e.to_string()));
        }
        RangeOutcome::Restart => {
            let _ = fs::remove_file(&partial);
        }
        RangeOutcome::Append => {}
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&partial)
        .map_err(|e| FetchError::Io(e.to_string()))?;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?
    {
        file.write_all(&chunk)
            .map_err(|e| FetchError::Io(e.to_string()))?;
    }
    drop(file);
    fs::rename(&partial, dest).map_err(|e| FetchError::Io(e.to_string()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn hf_urls_escape_each_segment_and_normalize_backslashes() {
        assert_eq!(
            hf_download_url("owner/model-GGUF", "sub\\My Model Q4_K_M.gguf"),
            "https://huggingface.co/owner/model-GGUF/resolve/main/sub/My%20Model%20Q4_K_M.gguf"
        );
        // Unreserved characters pass through untouched.
        assert_eq!(escape_segment("A-z_0.9~"), "A-z_0.9~");
        // Multi-byte UTF-8 escapes every byte.
        assert_eq!(escape_segment("é"), "%C3%A9");
    }

    #[test]
    fn resume_math_follows_the_partial_sidecar_contract() {
        assert_eq!(range_header(0), None);
        assert_eq!(range_header(1024).as_deref(), Some("bytes=1024-"));
        assert_eq!(
            partial_path(Path::new("/models/m.gguf")),
            PathBuf::from("/models/m.gguf.partial")
        );
    }

    #[test]
    fn range_status_classification_is_fail_closed() {
        assert_eq!(
            classify_range_status(200, false),
            Some(RangeOutcome::Restart)
        );
        // A server that ignores the range restarts the pull, never corrupts.
        assert_eq!(
            classify_range_status(200, true),
            Some(RangeOutcome::Restart)
        );
        assert_eq!(classify_range_status(206, true), Some(RangeOutcome::Append));
        // 206 without a requested range is out of contract.
        assert_eq!(classify_range_status(206, false), None);
        // 416 only means "already complete" when we actually resumed.
        assert_eq!(
            classify_range_status(416, true),
            Some(RangeOutcome::AlreadyComplete)
        );
        assert_eq!(classify_range_status(416, false), None);
        assert_eq!(classify_range_status(404, true), None);
        assert_eq!(classify_range_status(500, false), None);
    }
}
