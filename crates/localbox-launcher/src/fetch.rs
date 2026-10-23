//! Resumable HTTP download for model files, and the model-file download
//! targets a catalog entry implies.
//!
//! Mirrors the download contract users already rely on: a `.partial` sidecar
//! accumulates bytes, an interrupted pull resumes with a `Range` request, a
//! `416` answer means the partial is already the full file, and the final
//! name only ever appears fully written (rename-on-complete). A
//! `.partial.json` record of the remote file's identity (strong `ETag`, size)
//! sits beside the partial, so a pull only continues the file it was started
//! from and a file replaced upstream is fetched again rather than spliced.
//!
//! This lives in the launcher *library* so every consumer of the launcher
//! contract — the LocalBox binary and the LocalBench tuner alike — fetches a
//! model the same way, instead of one of them being able to and the other
//! having to tell the user to go run the first.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use localx_llama_core::LauncherError;

/// A download failure.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The HTTP request failed or answered outside the resume contract.
    #[error("download failed: {0}")]
    Http(String),
    /// Local file I/O failed.
    #[error("download file error: {0}")]
    Io(String),
    /// The transfer ended with a different byte count than the server
    /// declared for the file, so it was not put in place.
    #[error("download of {url} ended at {received} bytes, but the file is {expected} bytes")]
    SizeMismatch {
        url: String,
        received: u64,
        expected: u64,
    },
}

/// A model-file fetch failure: either the catalog could not name what to fetch,
/// or one file's download failed.
#[derive(Debug, thiserror::Error)]
pub enum ModelFetchError {
    /// The catalog cannot resolve the model, quant, or file to fetch.
    #[error("{0}")]
    Resolve(#[from] LauncherError),
    /// One artifact's download failed; the others before it are complete.
    #[error("{kind} for the model could not be downloaded to {}: {source}", path.display())]
    Download {
        kind: DownloadKind,
        path: PathBuf,
        #[source]
        source: FetchError,
    },
}

/// Which model artifact a download target is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DownloadKind {
    /// The main GGUF weights.
    Gguf,
    /// The multimodal projector (`VisionModule`).
    VisionModule,
    /// The speculative-decoding draft model (`DraftModule`).
    DraftModule,
}

impl DownloadKind {
    /// Every kind, in the order a launch needs them.
    pub const ALL: [DownloadKind; 3] = [
        DownloadKind::Gguf,
        DownloadKind::VisionModule,
        DownloadKind::DraftModule,
    ];

    /// The user-facing name of the artifact.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            DownloadKind::Gguf => "model",
            DownloadKind::VisionModule => "vision projector",
            DownloadKind::DraftModule => "draft model",
        }
    }
}

impl std::fmt::Display for DownloadKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// One file a catalog model needs on disk: where it comes from, where it goes,
/// and whether it is already there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDownload {
    pub kind: DownloadKind,
    pub url: String,
    pub path: PathBuf,
    /// The file is already on disk (a fetch skips it).
    pub present: bool,
}

/// One progress report during a model-file fetch: which artifact, where it is
/// landing, how many bytes of it are on disk so far, and the total when the
/// server said.
#[derive(Debug, Clone, Copy)]
pub struct DownloadProgress<'a> {
    pub kind: DownloadKind,
    pub path: &'a Path,
    pub received: u64,
    pub total: Option<u64>,
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
    let repo = repo
        .replace('\\', "/")
        .split('/')
        .map(escape_segment)
        .collect::<Vec<_>>()
        .join("/");
    let path = file_name
        .replace('\\', "/")
        .split('/')
        .map(escape_segment)
        .collect::<Vec<_>>()
        .join("/");
    format!("{}/{repo}/resolve/main/{path}", crate::hf_meta::endpoint())
}

/// The sidecar path bytes accumulate in until the download completes.
#[must_use]
pub fn partial_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    dest.with_file_name(name)
}

/// The record beside a `.partial` of which remote file its bytes came from
/// (`<file>.partial.json`). Written before the first byte lands and removed
/// once the file is in place.
#[must_use]
pub fn identity_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".partial.json");
    dest.with_file_name(name)
}

/// Which remote file a download's bytes come from: the server's strong
/// `ETag` (on Hugging Face's CDN, a hash of the content) and the full file
/// size. A partial is only continued from a response with the same identity,
/// so bytes of a file replaced upstream are never appended to an older prefix.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RemoteIdentity {
    #[serde(rename = "ETag", default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(rename = "Size", default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

impl RemoteIdentity {
    /// The identity a response declares under `outcome`. A weak `ETag`
    /// (`W/…`) only promises equivalent content, not the same bytes, so it
    /// identifies nothing here. The size is the `Content-Length` of a whole
    /// file, or the total after the `/` of a range's `Content-Range`.
    #[must_use]
    pub fn from_headers(headers: &reqwest::header::HeaderMap, outcome: RangeOutcome) -> Self {
        let text = |name: reqwest::header::HeaderName| {
            headers.get(name).and_then(|value| value.to_str().ok())
        };
        let etag = text(reqwest::header::ETAG)
            .map(str::trim)
            .filter(|tag| !tag.is_empty() && !tag.starts_with("W/"))
            .map(str::to_string);
        let size = match outcome {
            RangeOutcome::Restart => {
                text(reqwest::header::CONTENT_LENGTH).and_then(|value| value.trim().parse().ok())
            }
            RangeOutcome::Append | RangeOutcome::AlreadyComplete => {
                text(reqwest::header::CONTENT_RANGE).and_then(content_range_total)
            }
        };
        Self { etag, size }
    }

    /// Whether a ranged response continues the file `recorded` describes:
    /// the same strong `ETag`, and no disagreement about the size.
    #[must_use]
    pub fn continues(&self, recorded: &RemoteIdentity) -> bool {
        recorded.etag.is_some()
            && self.etag == recorded.etag
            && match (self.size, recorded.size) {
                (Some(now), Some(then)) => now == then,
                _ => true,
            }
    }

    /// Whether a `416` means the `existing` partial bytes already are the whole
    /// file `recorded` describes. A `416` rarely carries an `ETag`, so the
    /// recorded size has to match what is on disk.
    #[must_use]
    pub fn completes(&self, recorded: &RemoteIdentity, existing: u64) -> bool {
        recorded.size == Some(existing)
            && self.size.map_or(true, |size| size == existing)
            && self
                .etag
                .as_ref()
                .map_or(true, |tag| recorded.etag.as_ref() == Some(tag))
    }
}

/// The full file size after the `/` in a `Content-Range` value
/// (`bytes 100-199/907543008`, `bytes */907543008`); `None` when unknown (`*`).
#[must_use]
pub fn content_range_total(value: &str) -> Option<u64> {
    value.rsplit_once('/')?.1.trim().parse().ok()
}

fn read_identity(path: &Path) -> Option<RemoteIdentity> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn write_identity(path: &Path, identity: &RemoteIdentity) -> Result<(), FetchError> {
    let json = serde_json::to_vec(identity).map_err(|e| FetchError::Io(e.to_string()))?;
    fs::write(path, json).map_err(|e| FetchError::Io(e.to_string()))
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

/// The total file size implied by a response: what is already on disk plus
/// what the server still has to send (`Content-Length`), when it said. A
/// restart from zero counts nothing as already there.
#[must_use]
pub fn total_size(
    outcome: RangeOutcome,
    existing: u64,
    content_length: Option<u64>,
) -> Option<u64> {
    match outcome {
        RangeOutcome::Append => content_length.map(|remaining| existing.saturating_add(remaining)),
        RangeOutcome::Restart => content_length,
        RangeOutcome::AlreadyComplete => Some(existing),
    }
}

/// Download `url` to `dest` with append-resume via the `.partial` sidecar,
/// reporting `(bytes on disk so far, total when known)` after every chunk. The
/// destination appears only after all bytes landed; an existing `dest`
/// short-circuits as already downloaded.
///
/// A partial is only continued when its [`identity_path`] record matches the
/// identity the server declares for the range; otherwise (no record, or the
/// file was replaced upstream) it is downloaded again from zero. A transfer
/// whose byte count differs from the declared size is not put in place.
///
/// # Errors
/// [`FetchError`] on HTTP failure outside the resume contract, on file I/O,
/// or when the transfer does not match the declared size.
pub async fn download_with_resume(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    on_progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<(), FetchError> {
    if dest.is_file() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| FetchError::Io(e.to_string()))?;
    }
    let partial = partial_path(dest);
    let record = identity_path(dest);
    let existing = fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);
    // Only a partial whose remote file is known can be continued: without a
    // recorded strong ETag nothing shows its bytes belong to today's file.
    let recorded = read_identity(&record).filter(|identity| identity.etag.is_some());
    let resume_from = if recorded.is_some() { existing } else { 0 };

    let (mut response, mut outcome) = request_from(client, url, resume_from).await?;
    let mut identity = RemoteIdentity::from_headers(response.headers(), outcome);
    let continues = match (outcome, &recorded) {
        (RangeOutcome::Append, Some(recorded)) => identity.continues(recorded),
        (RangeOutcome::AlreadyComplete, Some(recorded)) => identity.completes(recorded, existing),
        _ => true,
    };
    if !continues {
        // The file was replaced upstream since the partial was started: the
        // range would come from a different file. Fetch the whole file again.
        drop(response);
        (response, outcome) = request_from(client, url, 0).await?;
        identity = RemoteIdentity::from_headers(response.headers(), outcome);
    }
    let total = identity
        .size
        .or_else(|| total_size(outcome, existing, response.content_length()));

    // One report before the first chunk, so a consumer sees the artifact start
    // (and, on a resume, how much was already there) even when the body is
    // slow to arrive.
    let mut received = match outcome {
        RangeOutcome::AlreadyComplete => {
            on_progress(existing, Some(existing));
            fs::rename(&partial, dest).map_err(|e| FetchError::Io(e.to_string()))?;
            let _ = fs::remove_file(&record);
            return Ok(());
        }
        RangeOutcome::Restart => 0,
        RangeOutcome::Append => existing,
    };
    on_progress(received, total);

    // A restart must never keep a stale prefix: truncate rather than rely on
    // a delete that could fail and leave the old bytes ahead of the new body.
    // Its record is written first, so an interrupted pull can be continued.
    let mut file = match outcome {
        RangeOutcome::Restart => {
            write_identity(&record, &identity)?;
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&partial)
        }
        RangeOutcome::Append | RangeOutcome::AlreadyComplete => {
            OpenOptions::new().create(true).append(true).open(&partial)
        }
    }
    .map_err(|e| FetchError::Io(e.to_string()))?;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?
    {
        file.write_all(&chunk)
            .map_err(|e| FetchError::Io(e.to_string()))?;
        received = received.saturating_add(chunk.len() as u64);
        on_progress(received, total);
    }
    drop(file);
    if let Some(expected) = total.filter(|expected| *expected != received) {
        // Short: the partial stays to be continued. Long: its bytes cannot be
        // the declared file, so it goes and the next pull starts clean.
        if received > expected {
            let _ = fs::remove_file(&partial);
            let _ = fs::remove_file(&record);
        }
        return Err(FetchError::SizeMismatch {
            url: url.to_string(),
            received,
            expected,
        });
    }
    fs::rename(&partial, dest).map_err(|e| FetchError::Io(e.to_string()))?;
    let _ = fs::remove_file(&record);
    Ok(())
}

/// One GET of `url`, ranged from `from` when it is past zero, classified under
/// the resume contract.
async fn request_from(
    client: &reqwest::Client,
    url: &str,
    from: u64,
) -> Result<(reqwest::Response, RangeOutcome), FetchError> {
    let mut request = client.get(url);
    if let Some(range) = range_header(from) {
        request = request.header(reqwest::header::RANGE, range);
    }
    let response = request
        .send()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?;
    let status = response.status().as_u16();
    let outcome = classify_range_status(status, from > 0)
        .ok_or_else(|| FetchError::Http(format!("{url}: unexpected HTTP status {status}")))?;
    Ok((response, outcome))
}

/// Human-readable byte count for progress lines: `3.8 GB`, `512.0 MB`.
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.1} GB", value / GB)
    } else if value >= MB {
        format!("{:.1} MB", value / MB)
    } else {
        format!("{bytes} B")
    }
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

    #[test]
    fn the_total_counts_the_partial_when_the_server_appends() {
        // A resumed pull's Content-Length is the remainder; the total is what is
        // already on disk plus that. A restart's Content-Length is the whole file.
        assert_eq!(
            total_size(RangeOutcome::Append, 1_000, Some(9_000)),
            Some(10_000)
        );
        assert_eq!(
            total_size(RangeOutcome::Restart, 1_000, Some(9_000)),
            Some(9_000)
        );
        assert_eq!(
            total_size(RangeOutcome::AlreadyComplete, 1_000, None),
            Some(1_000)
        );
        assert_eq!(total_size(RangeOutcome::Append, 1_000, None), None);
    }

    #[test]
    fn byte_counts_read_like_a_progress_line() {
        assert_eq!(human_bytes(15_247_099_617), "14.2 GB");
        assert_eq!(human_bytes(536_870_912), "512.0 MB");
        assert_eq!(human_bytes(42), "42 B");
    }

    /// One scripted answer of the loopback server: a status line, extra
    /// headers, and the body. `declared_length` overrides `Content-Length`,
    /// so a test can cut a transfer short.
    struct Reply {
        status: &'static str,
        headers: Vec<String>,
        body: &'static [u8],
        declared_length: Option<usize>,
    }

    fn reply(status: &'static str, headers: &[&str], body: &'static [u8]) -> Reply {
        Reply {
            status,
            headers: headers.iter().map(|h| (*h).to_string()).collect(),
            body,
            declared_length: None,
        }
    }

    /// Serve `replies` to successive connections; the handle yields each
    /// request head in order.
    fn serve(replies: Vec<Reply>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let mut heads = Vec::new();
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                heads.push(String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase());
                let mut head = format!(
                    "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    reply.status,
                    reply.declared_length.unwrap_or(reply.body.len())
                );
                for header in &reply.headers {
                    head.push_str(header);
                    head.push_str("\r\n");
                }
                head.push_str("\r\n");
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(reply.body).unwrap();
                let _ = stream.flush();
            }
            heads
        });
        (format!("http://{addr}/m.gguf"), handle)
    }

    fn record(dest: &Path, etag: &str, size: u64) {
        write_identity(
            &identity_path(dest),
            &RemoteIdentity {
                etag: Some(etag.to_string()),
                size: Some(size),
            },
        )
        .unwrap();
    }

    /// `(bytes on disk, total)` progress reports, in order.
    type Events = Vec<(u64, Option<u64>)>;

    fn fetch(url: &str, dest: &Path) -> (Result<(), FetchError>, Events) {
        let client = reqwest::Client::new();
        let mut events = Vec::new();
        let result = block_on(download_with_resume(&client, url, dest, &mut |r, t| {
            events.push((r, t));
        }));
        (result, events)
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn the_identity_record_sits_beside_the_partial() {
        assert_eq!(
            identity_path(Path::new("/models/m.gguf")),
            PathBuf::from("/models/m.gguf.partial.json")
        );
    }

    #[test]
    fn a_response_identity_is_its_strong_etag_and_full_size() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::ETAG, "\"abc\"".parse().unwrap());
        headers.insert(
            reqwest::header::CONTENT_RANGE,
            "bytes 100-199/907543008".parse().unwrap(),
        );
        headers.insert(reqwest::header::CONTENT_LENGTH, "100".parse().unwrap());
        let ranged = RemoteIdentity::from_headers(&headers, RangeOutcome::Append);
        assert_eq!(ranged.etag.as_deref(), Some("\"abc\""));
        // A range's size is the file's total, not the length of the range.
        assert_eq!(ranged.size, Some(907_543_008));
        let whole = RemoteIdentity::from_headers(&headers, RangeOutcome::Restart);
        assert_eq!(whole.size, Some(100));

        // A weak ETag promises equivalent content, not the same bytes.
        headers.insert(reqwest::header::ETAG, "W/\"abc\"".parse().unwrap());
        assert_eq!(
            RemoteIdentity::from_headers(&headers, RangeOutcome::Append).etag,
            None
        );
        assert_eq!(content_range_total("bytes */5"), Some(5));
        assert_eq!(content_range_total("bytes 0-4/*"), None);
    }

    #[test]
    fn a_range_continues_only_the_recorded_file() {
        let recorded = RemoteIdentity {
            etag: Some("\"v1\"".to_string()),
            size: Some(12),
        };
        let same = recorded.clone();
        assert!(same.continues(&recorded));
        let replaced = RemoteIdentity {
            etag: Some("\"v2\"".to_string()),
            size: Some(12),
        };
        assert!(!replaced.continues(&recorded));
        let resized = RemoteIdentity {
            etag: Some("\"v1\"".to_string()),
            size: Some(13),
        };
        assert!(!resized.continues(&recorded));
        let untagged = RemoteIdentity {
            etag: None,
            size: Some(12),
        };
        assert!(!untagged.continues(&recorded));
        // Without a recorded ETag nothing can be shown to continue.
        assert!(!same.continues(&RemoteIdentity::default()));
    }

    #[test]
    fn a_restart_never_keeps_a_stale_partial_prefix() {
        // A resume was requested (partial on disk) but the server answered 200:
        // the body is the whole file and the old bytes must not survive ahead
        // of it — even if the partial could not be deleted.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.gguf");
        std::fs::write(partial_path(&dest), b"STALEPREFIX").unwrap();
        record(&dest, "\"v1\"", 10);
        let (url, server) = serve(vec![reply("200 OK", &["ETag: \"v1\""], b"fresh-body")]);

        let (result, events) = fetch(&url, &dest);

        result.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"fresh-body");
        assert!(!partial_path(&dest).exists());
        assert!(!identity_path(&dest).exists());
        let heads = server.join().unwrap();
        assert!(
            heads[0].contains("range: bytes=11-"),
            "the resume was requested: {}",
            heads[0]
        );
        assert_eq!(
            events.first(),
            Some(&(0, Some(10))),
            "restart reports zero first"
        );
        assert_eq!(events.last(), Some(&(10, Some(10))));
    }

    #[test]
    fn a_resume_of_the_recorded_file_appends_and_reports_the_existing_bytes_first() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.gguf");
        std::fs::write(partial_path(&dest), b"first-").unwrap();
        record(&dest, "\"v1\"", 12);
        let (url, server) = serve(vec![reply(
            "206 Partial Content",
            &["ETag: \"v1\"", "Content-Range: bytes 6-11/12"],
            b"second",
        )]);

        let (result, events) = fetch(&url, &dest);

        result.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"first-second");
        assert!(
            !identity_path(&dest).exists(),
            "the record goes with the partial"
        );
        assert!(server.join().unwrap()[0].contains("range: bytes=6-"));
        // The initial report carries what was already on disk and the file's
        // total; the last one is the complete file.
        assert_eq!(events.first(), Some(&(6, Some(12))));
        assert_eq!(events.last(), Some(&(12, Some(12))));
    }

    #[test]
    fn a_file_replaced_upstream_is_downloaded_again_instead_of_appended() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.gguf");
        std::fs::write(partial_path(&dest), b"first-").unwrap();
        record(&dest, "\"v1\"", 12);
        let (url, server) = serve(vec![
            reply(
                "206 Partial Content",
                &["ETag: \"v2\"", "Content-Range: bytes 6-11/12"],
                b"XXXXXX",
            ),
            reply("200 OK", &["ETag: \"v2\""], b"brand-new-12"),
        ]);

        let (result, events) = fetch(&url, &dest);

        result.unwrap();
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"brand-new-12",
            "no byte of the old partial or the mismatched range survives"
        );
        let heads = server.join().unwrap();
        assert!(heads[0].contains("range: bytes=6-"));
        assert!(
            !heads[1].contains("range:"),
            "the retry asks for the whole file"
        );
        assert_eq!(events.first(), Some(&(0, Some(12))));
        assert!(!identity_path(&dest).exists());
    }

    #[test]
    fn a_partial_without_a_record_is_downloaded_again_from_zero() {
        // Every partial left by a build before the record existed: nothing
        // shows its bytes belong to the file the server has today.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.gguf");
        std::fs::write(partial_path(&dest), b"OLD").unwrap();
        let (url, server) = serve(vec![reply("200 OK", &["ETag: \"v1\""], b"new-body")]);

        let (result, _) = fetch(&url, &dest);

        result.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new-body");
        assert!(!server.join().unwrap()[0].contains("range:"));
    }

    #[test]
    fn an_interrupted_download_keeps_its_record_so_it_can_be_continued() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.gguf");
        let mut cut = reply("200 OK", &["ETag: \"v1\""], b"0123");
        cut.declared_length = Some(10);
        let (url, server) = serve(vec![cut]);

        let (result, _) = fetch(&url, &dest);

        assert!(result.is_err(), "a cut transfer is not a download");
        server.join().unwrap();
        assert!(!dest.exists());
        assert_eq!(std::fs::read(partial_path(&dest)).unwrap(), b"0123");
        assert_eq!(
            read_identity(&identity_path(&dest)),
            Some(RemoteIdentity {
                etag: Some("\"v1\"".to_string()),
                size: Some(10),
            })
        );
    }

    #[test]
    fn a_transfer_short_of_the_declared_size_is_not_put_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.gguf");
        std::fs::write(partial_path(&dest), b"first-").unwrap();
        record(&dest, "\"v1\"", 20);
        let (url, server) = serve(vec![reply(
            "206 Partial Content",
            &["ETag: \"v1\"", "Content-Range: bytes 6-11/20"],
            b"second",
        )]);

        let (result, _) = fetch(&url, &dest);

        server.join().unwrap();
        assert!(matches!(
            result,
            Err(FetchError::SizeMismatch {
                received: 12,
                expected: 20,
                ..
            })
        ));
        assert!(!dest.exists());
        // Short means unfinished: the partial and its record stay for a resume.
        assert_eq!(std::fs::read(partial_path(&dest)).unwrap(), b"first-second");
        assert!(identity_path(&dest).exists());
    }

    #[test]
    fn a_transfer_past_the_declared_size_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.gguf");
        std::fs::write(partial_path(&dest), b"first-").unwrap();
        record(&dest, "\"v1\"", 8);
        let (url, server) = serve(vec![reply(
            "206 Partial Content",
            &["ETag: \"v1\"", "Content-Range: bytes 6-7/8"],
            b"second",
        )]);

        let (result, _) = fetch(&url, &dest);

        server.join().unwrap();
        assert!(matches!(
            result,
            Err(FetchError::SizeMismatch {
                received: 12,
                expected: 8,
                ..
            })
        ));
        // Too long cannot be the declared file: nothing is kept to resume from.
        assert!(!dest.exists());
        assert!(!partial_path(&dest).exists());
        assert!(!identity_path(&dest).exists());
    }

    #[test]
    fn a_416_on_resume_completes_the_recorded_partial_and_reports_once() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.gguf");
        std::fs::write(partial_path(&dest), b"whole").unwrap();
        record(&dest, "\"v1\"", 5);
        let (url, server) = serve(vec![reply(
            "416 Range Not Satisfiable",
            &["Content-Range: bytes */5"],
            b"",
        )]);

        let (result, events) = fetch(&url, &dest);

        result.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"whole");
        server.join().unwrap();
        assert_eq!(events, vec![(5, Some(5))]);
        assert!(!identity_path(&dest).exists());
    }

    #[test]
    fn a_416_for_a_different_size_downloads_the_file_again() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.gguf");
        std::fs::write(partial_path(&dest), b"whole").unwrap();
        record(&dest, "\"v1\"", 5);
        let (url, server) = serve(vec![
            reply(
                "416 Range Not Satisfiable",
                &["Content-Range: bytes */3"],
                b"",
            ),
            reply("200 OK", &["ETag: \"v2\""], b"new"),
        ]);

        let (result, _) = fetch(&url, &dest);

        result.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        assert_eq!(server.join().unwrap().len(), 2);
    }

    #[test]
    fn a_fresh_download_reports_start_then_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.gguf");
        let (url, server) = serve(vec![reply("200 OK", &["ETag: \"v1\""], b"0123456789")]);

        let (result, events) = fetch(&url, &dest);

        result.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"0123456789");
        assert!(!server.join().unwrap()[0].contains("range:"));
        assert!(!identity_path(&dest).exists());
        assert_eq!(events.first(), Some(&(0, Some(10))));
        assert_eq!(events.last(), Some(&(10, Some(10))));
    }

    #[test]
    fn download_kinds_name_themselves_for_users() {
        assert_eq!(DownloadKind::Gguf.to_string(), "model");
        assert_eq!(DownloadKind::VisionModule.label(), "vision projector");
        assert_eq!(DownloadKind::DraftModule.label(), "draft model");
        assert_eq!(DownloadKind::ALL.len(), 3);
    }
}
