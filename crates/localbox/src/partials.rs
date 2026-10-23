//! Interrupted downloads under the GGUF root: every `<file>.partial` a stopped
//! transfer left behind, whether LocalBox will still continue it, and which
//! files `localbox clean --partials` may remove.
//!
//! [`scan`] is the only part that reads the disk; [`classify`] is pure over
//! what the scan found, the catalog, the quants in use, file sizes and the
//! time, so the rules are tested without a real GGUF root. Nothing here
//! deletes: the command layer removes what a [`CleanPlan`] lists, and only
//! when asked.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use localbox_launcher::catalog::Catalog;
use localbox_launcher::fetch::{human_bytes, identity_path, RemoteIdentity};
use localx_llama_core::quant::shard_files_from_primary;
use localx_llama_core::ModelDef;

/// Written to this recently, a partial is still being written by another
/// process — even one whose finished file is already there — so it is kept.
pub const IN_FLIGHT: Duration = Duration::from_secs(10 * 60);

/// Grown this recently, a partial counts as a download in progress, whatever
/// it belongs to (`download --quant x`, `findbest --quant x`).
pub const RECENT: Duration = Duration::from_secs(24 * 60 * 60);

/// Idle this long, a cleanup candidate is pointed out as stale. Age alone
/// never makes a partial a candidate: large downloads are paused for days.
pub const STALE: Duration = Duration::from_secs(14 * 24 * 60 * 60);

const SUFFIX: &str = ".partial";

fn target_of(partial: &Path) -> PathBuf {
    let name = partial.file_name().unwrap_or_default().to_string_lossy();
    partial.with_file_name(name.strip_suffix(SUFFIX).unwrap_or(&name))
}

/// One `.partial` file as found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundPartial {
    pub path: PathBuf,
    pub bytes: u64,
    pub modified: SystemTime,
    /// Its `.partial.json` record of the remote file, when it has one.
    pub record: Option<RemoteIdentity>,
}

impl FoundPartial {
    /// The file this partial becomes once complete.
    #[must_use]
    pub fn target(&self) -> PathBuf {
        target_of(&self.path)
    }

    fn resumable(&self) -> bool {
        self.record
            .as_ref()
            .is_some_and(|record| record.etag.is_some())
    }
}

/// What a partial is to LocalBox, in the order the rules decide it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PartialState {
    /// The finished file is already there; a download never touches it again.
    Superseded,
    /// It will be continued: written to recently, or part of what a launch or
    /// download of its model would fetch now.
    Active,
    /// It has no record of its remote file, so a download starts it over.
    Unresumable,
    /// A quant of a catalog model that is not the one in use.
    OtherQuant,
    /// No catalog model owns it.
    Orphan,
}

impl PartialState {
    /// Whether `clean --partials` removes it.
    #[must_use]
    pub fn is_candidate(self) -> bool {
        self != PartialState::Active
    }

    /// The one-line reason shown next to the partial.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            PartialState::Superseded => "superseded: the finished file is already there",
            PartialState::Active => "active: continues on the next download",
            PartialState::Unresumable => {
                "cannot resume safely: no record of the remote file, a download starts over"
            }
            PartialState::OtherQuant => "other quant: not the quant in use for this model",
            PartialState::Orphan => "orphan: no catalog model uses this file",
        }
    }
}

/// One partial, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialEntry {
    pub found: FoundPartial,
    pub state: PartialState,
    /// The catalog model whose folder it is in, when any.
    pub model: Option<String>,
    /// The quant it is a shard of, when any.
    pub quant: Option<String>,
    /// How long since it last grew.
    pub idle: Duration,
    /// Every file removing it takes, with its size: the partial and its
    /// record, and for an abandoned quant the finished shards of its now
    /// unusable set. Empty for an active partial.
    pub removes: Vec<(PathBuf, u64)>,
}

impl PartialEntry {
    /// Idle long enough to be pointed out as stale.
    #[must_use]
    pub fn is_stale(&self) -> bool {
        self.state.is_candidate() && self.idle >= STALE
    }
}

/// What a catalog model claims a file as.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Owner {
    /// One shard of a quant; `set` is every shard of that quant.
    Quant {
        model: String,
        quant: String,
        set: Vec<PathBuf>,
    },
    /// A file the model needs whatever its quant: projector, drafter, or a
    /// legacy single `File`.
    Always { model: String },
}

/// A path with its separators normalized, so a catalog spelling
/// (`IQ3_XXS/x.gguf`) and a scanned one compare equal on every platform.
fn normal(path: &Path) -> PathBuf {
    path.components().collect()
}

fn safe_folder(folder: &str) -> bool {
    let path = Path::new(folder);
    !folder.trim().is_empty()
        && path.is_relative()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
}

fn model_folder(root: &Path, key: &str, def: &ModelDef) -> Option<PathBuf> {
    let folder = def.root.as_deref().unwrap_or(key);
    safe_folder(folder).then(|| normal(&root.join(folder)))
}

/// Every file the catalog names under `root`, and every model folder.
fn owners(catalog: &Catalog, root: &Path) -> (BTreeMap<PathBuf, Owner>, BTreeMap<PathBuf, String>) {
    let mut files = BTreeMap::new();
    let mut folders = BTreeMap::new();
    for key in catalog.model_keys() {
        let Some(def) = catalog.model(key) else {
            continue;
        };
        let Some(folder) = model_folder(root, key, def) else {
            continue;
        };
        folders.insert(folder.clone(), key.to_string());
        for (quant, entry) in &def.quants {
            let set: Vec<PathBuf> = shard_files_from_primary(&entry.file)
                .iter()
                .map(|shard| normal(&folder.join(shard)))
                .collect();
            for shard in &set {
                files.insert(
                    shard.clone(),
                    Owner::Quant {
                        model: key.to_string(),
                        quant: quant.clone(),
                        set: set.clone(),
                    },
                );
            }
        }
        let always = [&def.file, &def.vision_module, &def.draft_module];
        for name in always.into_iter().flatten() {
            if !name.trim().is_empty() {
                files.insert(
                    normal(&folder.join(name)),
                    Owner::Always {
                        model: key.to_string(),
                    },
                );
            }
        }
    }
    (files, folders)
}

/// Which quants a model is using, per catalog key: its catalog `Quant`, plus
/// the quant saved in its launch defaults. `None` means every quant counts —
/// a model with no catalog default leaves the choice open.
pub type QuantsInUse = BTreeMap<String, Option<BTreeSet<String>>>;

/// The quants in use for every catalog model: always the catalog `Quant`
/// (what `download <key>`, a launch without saved defaults, and `findbest`
/// without `--quant` fetch) and the quant in the model's saved launch
/// defaults, when there is one. Keys are lowercase.
#[must_use]
pub fn quants_in_use(catalog: &Catalog) -> QuantsInUse {
    catalog
        .model_keys()
        .into_iter()
        .filter_map(|key| {
            let def = catalog.model(key)?;
            let Some(default) = def.quant.as_deref() else {
                return Some((key.to_string(), None));
            };
            let mut quants = BTreeSet::from([default.to_ascii_lowercase()]);
            let saved = crate::guided::load_default_launch(catalog, key);
            // Resolved the way a launch resolves it: a catalog key, any case.
            let saved_quant = (saved.model_key.as_deref() == Some(key))
                .then_some(saved.quant)
                .flatten()
                .and_then(|quant| {
                    def.quants
                        .keys()
                        .find(|known| known.eq_ignore_ascii_case(&quant))
                        .cloned()
                });
            if let Some(quant) = saved_quant {
                quants.insert(quant.to_ascii_lowercase());
            }
            Some((key.to_string(), Some(quants)))
        })
        .collect()
}

fn in_use(in_use: &QuantsInUse, model: &str, quant: &str) -> bool {
    match in_use.get(model) {
        Some(Some(quants)) => quants.contains(&quant.to_ascii_lowercase()),
        Some(None) | None => true,
    }
}

/// Classify what [`scan`] found. `file_size` answers whether a file exists
/// (and how big it is); `now` is the reference time for how long each partial
/// has been idle. Entries come back in path order.
#[must_use]
pub fn classify(
    catalog: &Catalog,
    root: &Path,
    found: Vec<FoundPartial>,
    using: &QuantsInUse,
    file_size: &dyn Fn(&Path) -> Option<u64>,
    now: SystemTime,
) -> Vec<PartialEntry> {
    let (files, folders) = owners(catalog, root);
    let mut entries: Vec<PartialEntry> = found
        .into_iter()
        .map(|found| {
            let idle = now.duration_since(found.modified).unwrap_or_default();
            let target = normal(&found.target());
            let owner = files.get(&target);
            let model = match owner {
                Some(Owner::Quant { model, .. } | Owner::Always { model }) => Some(model.clone()),
                None => target
                    .ancestors()
                    .find_map(|folder| folders.get(folder).cloned()),
            };
            let quant = match owner {
                Some(Owner::Quant { quant, .. }) => Some(quant.clone()),
                _ => None,
            };
            let state = if file_size(&target).is_some() {
                if idle < IN_FLIGHT {
                    PartialState::Active
                } else {
                    PartialState::Superseded
                }
            } else if idle < RECENT {
                PartialState::Active
            } else if !found.resumable() {
                PartialState::Unresumable
            } else {
                match owner {
                    Some(Owner::Always { .. }) => PartialState::Active,
                    Some(Owner::Quant { model, quant, .. }) if in_use(using, model, quant) => {
                        PartialState::Active
                    }
                    Some(Owner::Quant { .. }) => PartialState::OtherQuant,
                    None => PartialState::Orphan,
                }
            };
            let mut removes = Vec::new();
            if state.is_candidate() {
                removes.push((found.path.clone(), found.bytes));
                let record = identity_path(&found.target());
                if let Some(size) = file_size(&record) {
                    removes.push((record, size));
                }
                // An abandoned quant's finished shards cannot load without the
                // missing one, so they go with it.
                if let (PartialState::OtherQuant, Some(Owner::Quant { set, .. })) = (state, owner) {
                    for shard in set.iter().filter(|shard| **shard != target) {
                        if let Some(size) = file_size(shard) {
                            removes.push((shard.clone(), size));
                        }
                    }
                }
            }
            PartialEntry {
                found,
                state,
                model,
                quant,
                idle,
                removes,
            }
        })
        .collect();
    entries.sort_by(|a, b| a.found.path.cmp(&b.found.path));
    entries
}

/// Every `*.partial` file under `root`, with its size, age and record. A
/// missing root has none.
#[must_use]
pub fn scan(root: &Path) -> Vec<FoundPartial> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                pending.push(path);
            } else if path.to_string_lossy().ends_with(SUFFIX) {
                let record = std::fs::read(identity_path(&target_of(&path)))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                found.push(FoundPartial {
                    path,
                    bytes: meta.len(),
                    modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    record,
                });
            }
        }
    }
    found
}

/// The files `clean --partials` removes, each once, with the bytes it frees.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanPlan {
    pub files: Vec<(PathBuf, u64)>,
}

impl CleanPlan {
    #[must_use]
    pub fn from_entries(entries: &[PartialEntry]) -> Self {
        let mut seen = BTreeSet::new();
        let files = entries
            .iter()
            .flat_map(|entry| entry.removes.iter().cloned())
            .filter(|(path, _)| seen.insert(path.clone()))
            .collect();
        Self { files }
    }

    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|(_, size)| size).sum()
    }
}

fn idle_text(idle: Duration) -> String {
    let hours = idle.as_secs() / 3600;
    match hours {
        0 => "idle <1 h".to_string(),
        1..=47 => format!("idle {hours} h"),
        _ => format!("idle {} days", hours / 24),
    }
}

fn shown(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// One block per partial: where it is, how far it got, how long it has been
/// idle, and what it is.
#[must_use]
pub fn render(entries: &[PartialEntry], root: &Path) -> String {
    let mut out = String::new();
    for entry in entries {
        let owner = match (&entry.model, &entry.quant) {
            (Some(model), Some(quant)) => format!("{model} · {quant}"),
            (Some(model), None) => model.clone(),
            (None, _) => "no catalog model".to_string(),
        };
        let _ = writeln!(out, "  {}  ({owner})", shown(&entry.found.path, root));
        let progress = match entry.found.record.as_ref().and_then(|record| record.size) {
            Some(total) => format!(
                "{} of {}",
                human_bytes(entry.found.bytes),
                human_bytes(total)
            ),
            None => human_bytes(entry.found.bytes),
        };
        let stale = if entry.is_stale() { " · stale" } else { "" };
        // Kept for its age alone, it may belong to no download a launch would
        // make; say so rather than promise it continues.
        let reason = if entry.state == PartialState::Active && entry.idle < RECENT {
            "active: grew in the last 24 h, kept while its download may still continue"
        } else {
            entry.state.reason()
        };
        let _ = writeln!(
            out,
            "    {progress} · {}{stale} · {reason}",
            idle_text(entry.idle)
        );
    }
    out
}

/// The one-line pointer to cleanup when there is anything to clean.
#[must_use]
pub fn hint(entries: &[PartialEntry]) -> Option<String> {
    let candidates = entries.iter().filter(|e| e.state.is_candidate()).count();
    (candidates > 0).then(|| {
        let plan = CleanPlan::from_entries(entries);
        format!(
            "{candidates} partial download(s) LocalBox will not continue ({}): localbox clean --partials --check",
            human_bytes(plan.bytes())
        )
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const HOUR: Duration = Duration::from_secs(3600);
    const DAY: Duration = Duration::from_secs(24 * 3600);

    fn root() -> PathBuf {
        PathBuf::from("/gguf")
    }

    fn catalog() -> Catalog {
        catalog_with(&serde_json::Map::new())
    }

    fn catalog_with(settings: &serde_json::Map<String, serde_json::Value>) -> Catalog {
        let models = serde_json::json!({
            "Models": {
                "rco": {
                    "Repo": "owner/RCO-GGUF",
                    "Root": "rco",
                    "Quant": "q20",
                    "Quants": {
                        "q20":    { "File": "Q2_0/m-Q2_0-00001-of-00002.gguf" },
                        "iq3s":   { "File": "IQ3_S/m-IQ3_S-00001-of-00002.gguf" },
                        "iq3xxs": { "File": "IQ3_XXS/m-IQ3_XXS-00001-of-00002.gguf" }
                    },
                    "VisionModule": "mmproj.gguf",
                    "Contexts": { "": 8192 }
                },
                "open": {
                    "Repo": "owner/Open-GGUF",
                    "Quants": { "a": { "File": "a.gguf" }, "b": { "File": "b.gguf" } },
                    "Contexts": { "": 8192 }
                }
            }
        });
        let serde_json::Value::Object(models) = models else {
            unreachable!()
        };
        Catalog::from_layers(&serde_json::Map::new(), &models, settings).unwrap()
    }

    #[test]
    fn the_quants_in_use_are_the_catalog_default_and_the_saved_launch_quant() {
        let settings = serde_json::json!({
            "DefaultLaunchByModel": { "rco": { "ModelKey": "rco", "Quant": "IQ3S" } },
            "DefaultLaunch": { "ModelKey": "other", "Quant": "iq3xxs" }
        });
        let serde_json::Value::Object(settings) = settings else {
            unreachable!()
        };
        let using = quants_in_use(&catalog_with(&settings));
        assert_eq!(
            using["rco"],
            Some(BTreeSet::from(["q20".to_string(), "iq3s".to_string()])),
            "a recipe saved for another model does not count"
        );
        assert_eq!(
            using["open"], None,
            "no catalog default: every quant counts"
        );
        let plain = quants_in_use(&catalog());
        assert_eq!(plain["rco"], Some(BTreeSet::from(["q20".to_string()])));
    }

    fn using() -> QuantsInUse {
        BTreeMap::from([
            (
                "rco".to_string(),
                Some(BTreeSet::from(["q20".to_string(), "iq3s".to_string()])),
            ),
            ("open".to_string(), None),
        ])
    }

    fn partial(relative: &str, bytes: u64, idle: Duration, record: bool) -> FoundPartial {
        FoundPartial {
            path: root().join(format!("{relative}.partial")),
            bytes,
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000) - idle,
            record: record.then(|| RemoteIdentity {
                etag: Some("\"e\"".to_string()),
                size: Some(bytes * 2),
            }),
        }
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000)
    }

    fn run(found: Vec<FoundPartial>, on_disk: &[(&str, u64)]) -> Vec<PartialEntry> {
        let disk: HashMap<PathBuf, u64> = on_disk
            .iter()
            .map(|(relative, size)| (normal(&root().join(relative)), *size))
            .collect();
        classify(
            &catalog(),
            &root(),
            found,
            &using(),
            &|path| disk.get(&normal(path)).copied(),
            now(),
        )
    }

    fn state_of(found: FoundPartial, on_disk: &[(&str, u64)]) -> PartialState {
        run(vec![found], on_disk)[0].state
    }

    #[test]
    fn a_partial_beside_its_finished_file_is_superseded_whatever_its_quant() {
        let found = partial("rco/IQ3_S/m-IQ3_S-00001-of-00002.gguf", 5, 3 * DAY, true);
        let entries = run(
            vec![found],
            &[("rco/IQ3_S/m-IQ3_S-00001-of-00002.gguf", 10)],
        );
        assert_eq!(entries[0].state, PartialState::Superseded);
        assert_eq!(
            entries[0].quant.as_deref(),
            Some("iq3s"),
            "even the quant in use"
        );
        assert_eq!(
            entries[0].removes.len(),
            1,
            "only the partial, never the finished file"
        );
    }

    #[test]
    fn a_superseded_partial_still_being_written_is_kept() {
        let found = partial(
            "rco/IQ3_S/m-IQ3_S-00001-of-00002.gguf",
            5,
            IN_FLIGHT / 2,
            true,
        );
        let state = state_of(found, &[("rco/IQ3_S/m-IQ3_S-00001-of-00002.gguf", 10)]);
        assert_eq!(state, PartialState::Active);
    }

    #[test]
    fn a_partial_of_a_quant_in_use_is_active() {
        // The catalog default and the saved launch quant both count.
        for relative in [
            "rco/Q2_0/m-Q2_0-00001-of-00002.gguf",
            "rco/IQ3_S/m-IQ3_S-00002-of-00002.gguf",
        ] {
            assert_eq!(
                state_of(partial(relative, 5, 30 * DAY, true), &[]),
                PartialState::Active,
                "{relative}"
            );
        }
        assert_eq!(
            state_of(partial("rco/mmproj.gguf", 5, 30 * DAY, true), &[]),
            PartialState::Active,
            "a projector is needed whatever the quant"
        );
    }

    #[test]
    fn a_model_without_a_catalog_default_keeps_every_quant() {
        let found = partial("open/b.gguf", 5, 30 * DAY, true);
        assert_eq!(state_of(found, &[]), PartialState::Active);
    }

    #[test]
    fn a_recently_grown_partial_is_active_whatever_it_belongs_to() {
        for relative in ["rco/IQ3_XXS/m-IQ3_XXS-00001-of-00002.gguf", "gone/x.gguf"] {
            let found = partial(relative, 5, RECENT - HOUR, false);
            assert_eq!(state_of(found, &[]), PartialState::Active, "{relative}");
        }
    }

    #[test]
    fn a_partial_kept_for_its_age_says_so() {
        let entries = run(
            vec![partial(
                "rco/IQ3_XXS/m-IQ3_XXS-00001-of-00002.gguf",
                5,
                HOUR,
                false,
            )],
            &[],
        );
        let text = render(&entries, &root());
        assert!(
            text.contains("idle 1 h · active: grew in the last 24 h"),
            "{text}"
        );
    }

    #[test]
    fn a_partial_without_a_record_cannot_be_resumed_safely() {
        let found = partial("rco/Q2_0/m-Q2_0-00001-of-00002.gguf", 5, 2 * DAY, false);
        let entries = run(vec![found], &[]);
        assert_eq!(entries[0].state, PartialState::Unresumable);
        // The finished sibling shard of a quant in use is still good: kept.
        assert_eq!(entries[0].removes.len(), 1);
    }

    #[test]
    fn an_abandoned_quant_takes_its_finished_shards_with_it() {
        let found = partial(
            "rco/IQ3_XXS/m-IQ3_XXS-00002-of-00002.gguf",
            5,
            2 * DAY,
            true,
        );
        let entries = run(
            vec![found],
            &[
                ("rco/IQ3_XXS/m-IQ3_XXS-00001-of-00002.gguf", 40),
                ("rco/IQ3_XXS/m-IQ3_XXS-00002-of-00002.gguf.partial.json", 1),
            ],
        );
        assert_eq!(entries[0].state, PartialState::OtherQuant);
        assert_eq!(entries[0].model.as_deref(), Some("rco"));
        let removed: Vec<String> = entries[0]
            .removes
            .iter()
            .map(|(path, _)| shown(path, &root()))
            .collect();
        assert_eq!(
            removed,
            [
                "rco/IQ3_XXS/m-IQ3_XXS-00002-of-00002.gguf.partial",
                "rco/IQ3_XXS/m-IQ3_XXS-00002-of-00002.gguf.partial.json",
                "rco/IQ3_XXS/m-IQ3_XXS-00001-of-00002.gguf",
            ]
        );
    }

    #[test]
    fn a_partial_no_catalog_model_uses_is_an_orphan() {
        let in_unknown_folder = run(vec![partial("gone/x.gguf", 5, 2 * DAY, true)], &[]);
        assert_eq!(in_unknown_folder[0].state, PartialState::Orphan);
        assert_eq!(in_unknown_folder[0].model, None);
        let unknown_file = run(vec![partial("rco/old-name.gguf", 5, 2 * DAY, true)], &[]);
        assert_eq!(unknown_file[0].state, PartialState::Orphan);
        assert_eq!(unknown_file[0].model.as_deref(), Some("rco"));
    }

    #[test]
    fn the_plan_removes_each_file_once_and_counts_what_it_frees() {
        let entries = run(
            vec![
                partial(
                    "rco/IQ3_XXS/m-IQ3_XXS-00001-of-00002.gguf",
                    4,
                    2 * DAY,
                    true,
                ),
                partial(
                    "rco/IQ3_XXS/m-IQ3_XXS-00002-of-00002.gguf",
                    6,
                    2 * DAY,
                    true,
                ),
                partial("rco/Q2_0/m-Q2_0-00001-of-00002.gguf", 7, 2 * DAY, true),
            ],
            &[],
        );
        let plan = CleanPlan::from_entries(&entries);
        assert_eq!(plan.files.len(), 2, "the active Q2_0 partial is not in it");
        assert_eq!(plan.bytes(), 10);
        let hint = hint(&entries).unwrap();
        assert!(hint.starts_with("2 partial download(s)"), "{hint}");
        assert!(hint.contains("localbox clean --partials --check"), "{hint}");
    }

    #[test]
    fn nothing_to_clean_gives_no_hint() {
        let entries = run(
            vec![partial(
                "rco/Q2_0/m-Q2_0-00001-of-00002.gguf",
                7,
                2 * DAY,
                true,
            )],
            &[],
        );
        assert_eq!(hint(&entries), None);
    }

    #[test]
    fn stale_is_only_a_label_on_candidates() {
        let entries = run(
            vec![
                partial("gone/x.gguf", 5, STALE + DAY, true),
                partial("rco/Q2_0/m-Q2_0-00001-of-00002.gguf", 5, STALE + DAY, true),
            ],
            &[],
        );
        let text = render(&entries, &root());
        assert!(entries[0].is_stale());
        assert!(!entries[1].is_stale(), "an active partial is never stale");
        assert!(
            text.contains("gone/x.gguf.partial  (no catalog model)"),
            "{text}"
        );
        assert!(
            text.contains("5 B of 10 B · idle 15 days · stale · orphan"),
            "{text}"
        );
    }

    #[test]
    fn scan_finds_partials_with_their_records() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("rco").join("IQ3_XXS");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("m.gguf.partial"), b"1234").unwrap();
        std::fs::write(
            folder.join("m.gguf.partial.json"),
            br#"{"ETag":"\"e\"","Size":9}"#,
        )
        .unwrap();
        std::fs::write(folder.join("done.gguf"), b"x").unwrap();

        let found = scan(dir.path());

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].bytes, 4);
        assert_eq!(found[0].target(), folder.join("m.gguf"));
        assert_eq!(
            found[0].record,
            Some(RemoteIdentity {
                etag: Some("\"e\"".to_string()),
                size: Some(9),
            })
        );
        assert!(scan(&dir.path().join("missing")).is_empty());
    }
}
