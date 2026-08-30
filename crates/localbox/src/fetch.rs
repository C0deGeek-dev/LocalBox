//! Model-file downloads for the binary: the downloader itself lives in the
//! launcher library ([`localbox_launcher::fetch`]) so LocalBench and any other
//! consumer of the launcher contract fetches models exactly the way a launch
//! does; this module re-exports it and adds the terminal progress line.

use std::io::{IsTerminal, Write as _};
use std::path::PathBuf;

pub use localbox_launcher::fetch::{
    download_with_resume, hf_download_url, human_bytes, DownloadKind, DownloadProgress, FetchError,
    ModelDownload, ModelFetchError,
};

/// Renders download progress to stderr: a single line rewritten in place on a
/// terminal (`Downloading model m.gguf … 3.8 GB / 14.2 GB (27%)`), or one plain
/// line when each artifact starts (and one when it completes) when stderr is
/// not a terminal, so logs stay readable. Stateful so that a resumed download —
/// whose first report is not zero bytes — is still announced exactly once.
#[derive(Default)]
pub struct ProgressPrinter {
    announced: Option<PathBuf>,
}

impl ProgressPrinter {
    /// Report one progress event.
    pub fn report(&mut self, progress: &DownloadProgress<'_>) {
        let mut err = std::io::stderr();
        let terminal = err.is_terminal();
        for line in self.lines(progress, terminal) {
            let _ = write!(err, "{line}");
        }
        let _ = err.flush();
    }

    /// The text to emit for one event — separated from the terminal so the
    /// sequence is testable. On a terminal each event rewrites the line in
    /// place (`\r`) and the completing event ends it; off a terminal only the
    /// first event for an artifact and its completion print, each on its own
    /// line.
    fn lines(&mut self, progress: &DownloadProgress<'_>, terminal: bool) -> Vec<String> {
        let name = progress
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let first = self.announced.as_deref() != Some(progress.path);
        if first {
            self.announced = Some(progress.path.to_path_buf());
        }
        let complete = progress
            .total
            .is_some_and(|total| progress.received >= total);
        let line = format_progress_line(progress.kind, &name, progress.received, progress.total);
        if terminal {
            let mut out = vec![format!("\r{line}")];
            if complete {
                out.push("\n".to_string());
            }
            return out;
        }
        let mut out = Vec::new();
        if first {
            out.push(format!("Downloading {} {name} …\n", progress.kind));
        }
        if complete {
            out.push(format!(
                "Downloaded {} {name} ({})\n",
                progress.kind,
                human_bytes(progress.received)
            ));
        }
        out
    }
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
    use std::path::Path;

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

    #[test]
    fn off_terminal_a_resumed_artifact_is_announced_once_and_closed_once() {
        let mut printer = ProgressPrinter::default();
        let path = Path::new("m.gguf");
        let event = |received, total| DownloadProgress {
            kind: DownloadKind::Gguf,
            path,
            received,
            total: Some(total),
        };
        // A resume starts at the existing byte count, not zero.
        assert_eq!(
            printer.lines(&event(6, 12), false),
            vec!["Downloading model m.gguf …\n".to_string()]
        );
        assert!(printer.lines(&event(9, 12), false).is_empty());
        assert_eq!(
            printer.lines(&event(12, 12), false),
            vec!["Downloaded model m.gguf (12 B)\n".to_string()]
        );
        // The next artifact is announced anew.
        let next = DownloadProgress {
            kind: DownloadKind::DraftModule,
            path: Path::new("d.gguf"),
            received: 0,
            total: None,
        };
        assert_eq!(
            printer.lines(&next, false),
            vec!["Downloading draft model d.gguf …\n".to_string()]
        );
    }

    #[test]
    fn on_a_terminal_every_event_rewrites_the_line_and_completion_ends_it() {
        let mut printer = ProgressPrinter::default();
        let path = Path::new("m.gguf");
        let mid = DownloadProgress {
            kind: DownloadKind::Gguf,
            path,
            received: 5,
            total: Some(10),
        };
        assert_eq!(
            printer.lines(&mid, true),
            vec!["\rDownloading model m.gguf … 5 B / 10 B (50%)".to_string()]
        );
        let done = DownloadProgress {
            received: 10,
            ..mid
        };
        assert_eq!(
            printer.lines(&done, true),
            vec![
                "\rDownloading model m.gguf … 10 B / 10 B (100%)".to_string(),
                "\n".to_string()
            ]
        );
    }
}
