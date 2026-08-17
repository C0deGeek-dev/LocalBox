//! Model-file downloads for the binary: the downloader itself lives in the
//! launcher library ([`localbox_launcher::fetch`]) so LocalBench and any other
//! consumer of the launcher contract fetches models exactly the way a launch
//! does; this module re-exports it and adds the terminal progress line.

use std::io::{IsTerminal, Write as _};

pub use localbox_launcher::fetch::{
    download_with_resume, hf_download_url, human_bytes, DownloadKind, DownloadProgress, FetchError,
    ModelDownload, ModelFetchError,
};

/// Print one artifact's download progress to stderr: a single line rewritten
/// in place on a terminal (`downloading model … 3.8 GB / 14.2 GB (27%)`), or a
/// plain line per artifact start when stderr is not a terminal, so logs stay
/// readable.
pub fn print_progress(progress: &DownloadProgress<'_>) {
    let mut err = std::io::stderr();
    let name = progress
        .path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if !err.is_terminal() {
        if progress.received == 0 {
            let _ = writeln!(err, "Downloading {} {name} …", progress.kind);
        }
        return;
    }
    let line = format_progress_line(progress.kind, &name, progress.received, progress.total);
    let _ = write!(err, "\r{line}");
    if progress
        .total
        .is_some_and(|total| progress.received >= total)
    {
        let _ = writeln!(err);
    }
    let _ = err.flush();
}

/// The progress line for one artifact.
#[must_use]
pub fn format_progress_line(
    kind: DownloadKind,
    name: &str,
    received: u64,
    total: Option<u64>,
) -> String {
    match total {
        Some(total) if total > 0 => {
            let percent = received.saturating_mul(100) / total;
            format!(
                "Downloading {kind} {name} … {} / {} ({percent}%)",
                human_bytes(received),
                human_bytes(total)
            )
        }
        _ => format!("Downloading {kind} {name} … {}", human_bytes(received)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_progress_line_names_the_artifact_and_the_fraction() {
        assert_eq!(
            format_progress_line(
                DownloadKind::Gguf,
                "m.gguf",
                4_080_218_931,
                Some(15_247_099_617)
            ),
            "Downloading model m.gguf … 3.8 GB / 14.2 GB (26%)"
        );
        assert_eq!(
            format_progress_line(DownloadKind::DraftModule, "d.gguf", 1_048_576, None),
            "Downloading draft model d.gguf … 1.0 MB"
        );
    }
}
