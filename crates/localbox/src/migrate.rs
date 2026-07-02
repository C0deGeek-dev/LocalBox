//! Leftovers from a 1.x (PowerShell-era) install, and the plain-language
//! remedy. The 2.0.0 rewrite retired the PowerShell launcher, but a 1.x
//! `install.ps1` deployment survives the upgrade: a symlink-mode install
//! leaves dangling links (a red error at every shell start), a copy-mode
//! install keeps running the stale 1.x launcher silently. Nothing else
//! cleans those up — `install.ps1` itself is gone — so the binary detects
//! them and names the remedy.

use std::path::{Path, PathBuf};

/// One leftover artifact from a 1.x PowerShell-era install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V1Leftover {
    /// Where the leftover lives.
    pub path: PathBuf,
    /// What it is, in plain language (fits after the path on one line).
    pub what: &'static str,
}

/// PowerShell profile files (relative to home) that a 1.x
/// `install.ps1 -SetupProfile` may have wired to dot-source the retired
/// launcher. Best effort: a relocated Documents folder is not chased.
const PROFILE_CANDIDATES: [&str; 6] = [
    "Documents/PowerShell/profile.ps1",
    "Documents/PowerShell/Microsoft.PowerShell_profile.ps1",
    "Documents/WindowsPowerShell/profile.ps1",
    "Documents/WindowsPowerShell/Microsoft.PowerShell_profile.ps1",
    ".config/powershell/profile.ps1",
    ".config/powershell/Microsoft.PowerShell_profile.ps1",
];

/// Every 1.x leftover found under `home`: the deployed launcher file and
/// module tree in `~/.local-llm` (a dangling symlink counts — that is the
/// symlink-mode failure), and any PowerShell profile that still references
/// `LocalLLMProfile.ps1`.
#[must_use]
pub fn find_v1_leftovers(home: &Path) -> Vec<V1Leftover> {
    let mut found = Vec::new();
    // symlink_metadata, not exists(): a dangling symlink is still an entry
    // even though the path it points at is gone.
    let launcher = home.join(".local-llm").join("LocalLLMProfile.ps1");
    if launcher.symlink_metadata().is_ok() {
        found.push(V1Leftover {
            path: launcher,
            what: "the retired 1.x launcher file (or a dangling link to it)",
        });
    }
    let lib = home.join(".local-llm").join("lib");
    if lib.symlink_metadata().is_ok() {
        found.push(V1Leftover {
            path: lib,
            what: "the retired 1.x launcher module folder",
        });
    }
    for relative in PROFILE_CANDIDATES {
        // Component-wise join so the reported path uses native separators.
        let path = relative
            .split('/')
            .fold(home.to_path_buf(), |p, c| p.join(c));
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if content.contains("LocalLLMProfile.ps1") {
            found.push(V1Leftover {
                path,
                what: "a PowerShell profile that still loads the retired 1.x launcher",
            });
        }
    }
    found
}

/// The bounded plain-language notice for the found leftovers: what was
/// found, why it matters, and the remedy. Empty when nothing was found.
#[must_use]
pub fn v1_leftover_notice(leftovers: &[V1Leftover]) -> String {
    if leftovers.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "A LocalBox 1.x (PowerShell) install left files behind — \
         that launcher was replaced by this binary in 2.0.0:\n",
    );
    for item in leftovers {
        out.push_str(&format!("  - {} — {}\n", item.path.display(), item.what));
    }
    out.push_str(
        "  remedy: delete the leftover files and remove the LocalLLMProfile.ps1 \
         line from your PowerShell profile — see \"Upgrading from 1.x\" in \
         docs/install.md.",
    );
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn touch(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn a_clean_home_reports_nothing() {
        let home = tempfile::tempdir().unwrap();
        assert!(find_v1_leftovers(home.path()).is_empty());
        assert_eq!(v1_leftover_notice(&[]), "");
    }

    #[test]
    fn a_v2_home_without_1x_artifacts_reports_nothing() {
        // The seeded 2.x tree (catalog + defaults) is not a leftover.
        let home = tempfile::tempdir().unwrap();
        touch(&home.path().join(".local-llm/llm-models.json"), "{}");
        touch(&home.path().join(".local-llm/defaults.json"), "{}");
        assert!(find_v1_leftovers(home.path()).is_empty());
    }

    #[test]
    fn deployed_launcher_file_and_module_folder_are_found() {
        let home = tempfile::tempdir().unwrap();
        touch(&home.path().join(".local-llm/LocalLLMProfile.ps1"), "# v1");
        touch(&home.path().join(".local-llm/lib/00-settings.ps1"), "# v1");
        let found = find_v1_leftovers(home.path());
        assert_eq!(found.len(), 2);
        assert!(found[0].path.ends_with("LocalLLMProfile.ps1"));
        assert!(found[1].path.ends_with("lib"));
    }

    #[test]
    fn a_profile_that_dot_sources_the_retired_launcher_is_found() {
        let home = tempfile::tempdir().unwrap();
        touch(
            &home.path().join("Documents/PowerShell/profile.ps1"),
            ". \"$HOME\\.local-llm\\LocalLLMProfile.ps1\"",
        );
        // A profile without the reference stays quiet.
        touch(
            &home
                .path()
                .join("Documents/PowerShell/Microsoft.PowerShell_profile.ps1"),
            "Set-Alias ll Get-ChildItem",
        );
        let found = find_v1_leftovers(home.path());
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("profile.ps1"));
    }

    #[test]
    fn the_notice_names_every_path_and_the_remedy() {
        let home = tempfile::tempdir().unwrap();
        touch(&home.path().join(".local-llm/LocalLLMProfile.ps1"), "# v1");
        touch(&home.path().join(".local-llm/lib/x.ps1"), "# v1");
        let found = find_v1_leftovers(home.path());
        let notice = v1_leftover_notice(&found);
        assert!(notice.contains("1.x"));
        assert!(notice.contains("LocalLLMProfile.ps1"));
        assert!(notice.contains("remedy:"));
        assert!(notice.contains("Upgrading from 1.x"));
        for item in &found {
            assert!(notice.contains(&item.path.display().to_string()));
        }
    }
}
