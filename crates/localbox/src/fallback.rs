//! Engine backend fallback: when the installed native llama.cpp build cannot
//! load a model or answer, try the next build (CUDA, then Vulkan, then CPU)
//! without ever losing the build that was there before.
//!
//! The installed tree is moved aside once, kept across every attempt, and
//! restored when no candidate passes. A small journal next to the install
//! root records which tree and which settings are the original, so a run
//! interrupted at any point is put back into one consistent state — the
//! original tree with the original settings, or a verified replacement with
//! its own — and never a mix. Every engine mutation holds one exclusive,
//! OS-held lock, so a live run is never mistaken for an interrupted one.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::update::{read_stamp_tag, read_stamp_variant, Variant, NATIVE_VARIANT_SETTING};

/// The settings key holding the backend the last successful probe verified.
pub const VERIFIED_BACKEND_SETTING: &str = "LlamaCppVerifiedBackend";

/// The settings key holding the build an automatic fallback moved to, with
/// the NVIDIA driver it was decided on. It is not a user choice: later
/// updates keep the fallback build only while the driver is the same one, so
/// a driver fix brings the faster build back.
pub const FALLBACK_SETTING: &str = "LlamaCppNativeFallback";

/// The journal's file name, beside the engine install roots.
pub const JOURNAL_FILE: &str = ".localbox-fallback.json";

/// The lock file every engine mutation holds, beside the install roots.
pub const LOCK_FILE: &str = ".localbox-engine.lock";

/// The settings keys a fallback may change, and therefore snapshots.
pub const SNAPSHOT_KEYS: [&str; 5] = [
    NATIVE_VARIANT_SETTING,
    FALLBACK_SETTING,
    "LlamaCppPinnedTag",
    "LlamaCppDownloadPins",
    VERIFIED_BACKEND_SETTING,
];

/// A settings layer: the JSON object in `settings.json`.
pub type Settings = serde_json::Map<String, serde_json::Value>;

/// The identity an install carries in its build stamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub tag: String,
    pub variant: String,
}

impl Stamp {
    /// The stamp of the tree at `root`, when it has a complete one.
    #[must_use]
    pub fn read(root: &Path) -> Option<Self> {
        let tag = read_stamp_tag(root)?;
        let variant = read_stamp_variant(root)?;
        (!tag.trim().is_empty() && !variant.is_empty()).then(|| Self {
            tag: tag.trim().to_string(),
            variant,
        })
    }
}

/// Where a fallback run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Nothing is decided: recovery puts the original back.
    Pending,
    /// A candidate passed and its settings are written: recovery only
    /// finishes the cleanup.
    Committed,
}

/// The durable record of one fallback run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    pub phase: Phase,
    pub root: PathBuf,
    pub previous: PathBuf,
    pub original: Stamp,
    pub candidate: Option<Stamp>,
    /// The original values of [`SNAPSHOT_KEYS`] (absent keys stay absent).
    pub settings: Settings,
    /// A deletion in progress, if any.
    #[serde(default)]
    pub discarding: Option<Discard>,
}

/// The path the original tree is kept at during a run.
#[must_use]
pub fn previous_path(root: &Path) -> PathBuf {
    let name = root
        .file_name()
        .map_or_else(|| "engine".into(), |n| n.to_string_lossy().into_owned());
    root.with_file_name(format!("{name}.previous"))
}

/// The journal path for an install root.
#[must_use]
pub fn journal_path(root: &Path) -> PathBuf {
    root.with_file_name(JOURNAL_FILE)
}

/// Run `work` while holding the engine lock in `dir`. A second LocalBox
/// command changing the engine at the same time is refused, never queued.
/// The operating system releases the lock when the process ends, however it
/// ends, so there is no stale lock to reclaim.
///
/// # Errors
/// A plain message when the lock is held by another command or cannot be
/// opened, or whatever `work` returns.
pub fn with_engine_lock<T>(
    dir: &Path,
    work: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let path = dir.join(LOCK_FILE);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| format!("could not open the engine lock {}: {e}", path.display()))?;
    let mut lock = fd_lock::RwLock::new(file);
    let _held = match lock.try_write() {
        Ok(guard) => guard,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            return Err(format!(
                "another LocalBox command is changing the engine right now (lock {}); \
                 wait for it to finish, then run this again",
                path.display()
            ))
        }
        Err(e) => {
            return Err(format!(
                "could not take the engine lock {}: {e}",
                path.display()
            ))
        }
    };
    work()
}

/// Write `bytes` to `path` so a reader sees the old content or the new
/// content, never a torn file.
///
/// # Errors
/// The I/O error, as a plain message.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)
        .map_err(|e| format!("could not stage {}: {e}", path.display()))?;
    temp.write_all(bytes)
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|e| format!("could not stage {}: {e}", path.display()))?;
    temp.persist(path)
        .map_err(|e| format!("could not write {}: {}", path.display(), e.error))?;
    Ok(())
}

/// Read a settings layer; a missing file is an empty layer.
///
/// # Errors
/// A plain message when the file exists but is not a JSON object.
pub fn read_settings(path: &Path) -> Result<Settings, String> {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(raw.trim_start_matches('\u{feff}'))
            .map_err(|e| format!("{} is not valid JSON ({e}); fix it first", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::new()),
        Err(e) => Err(format!("could not read {}: {e}", path.display())),
    }
}

/// Write a settings layer atomically, pretty-printed like every other writer.
///
/// # Errors
/// A plain message when it cannot be written.
pub fn write_settings(path: &Path, settings: &Settings) -> Result<(), String> {
    let pretty = serde_json::to_string_pretty(&serde_json::Value::Object(settings.clone()))
        .map_err(|e| e.to_string())?;
    write_atomically(path, (pretty + "\n").as_bytes())
}

/// The values of [`SNAPSHOT_KEYS`] present in `settings`.
#[must_use]
pub fn snapshot(settings: &Settings) -> Settings {
    SNAPSHOT_KEYS
        .iter()
        .filter_map(|key| settings.get(*key).map(|v| ((*key).to_string(), v.clone())))
        .collect()
}

/// `current` with every snapshot key put back to its snapshot value (or
/// removed when it was absent). Other keys are left as they are.
#[must_use]
pub fn restore_snapshot(current: &Settings, snapshot: &Settings) -> Settings {
    let mut restored = current.clone();
    for key in SNAPSHOT_KEYS {
        match snapshot.get(key) {
            Some(value) => {
                restored.insert(key.to_string(), value.clone());
            }
            None => {
                restored.remove(key);
            }
        }
    }
    restored
}

fn write_journal(journal: &Journal) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(journal).map_err(|e| e.to_string())?;
    write_atomically(&journal_path(&journal.root), &bytes)
}

fn read_journal(root: &Path) -> Result<Option<Journal>, String> {
    let path = journal_path(root);
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).map(Some).map_err(|e| {
            format!(
                "the fallback journal {} cannot be read ({e}); nothing was changed — \
                 inspect it, the engine at {} and {}, then remove the journal",
                path.display(),
                root.display(),
                previous_path(root).display()
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("could not read {}: {e}", path.display())),
    }
}

fn remove_journal(root: &Path) -> Result<(), String> {
    let path = journal_path(root);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("could not remove {}: {e}", path.display())),
    }
}

/// A tree being deleted: moved from `from` to the journal-owned slot `to`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discard {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// A fresh, unique slot beside `root` to move a tree into before deleting it.
fn new_discard_slot(root: &Path) -> PathBuf {
    let name = root
        .file_name()
        .map_or_else(|| "engine".into(), |n| n.to_string_lossy().into_owned());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut n = 0u32;
    loop {
        let slot =
            root.with_file_name(format!("{name}.discard-{}-{nanos}-{n}", std::process::id()));
        if std::fs::symlink_metadata(&slot).is_err() {
            return slot;
        }
        n += 1;
    }
}

/// Whether `slot` is a discard slot this journal could have created: a
/// sibling of the install root named `<root>.discard-…`.
fn is_discard_slot(root: &Path, slot: &Path) -> bool {
    let (Some(name), Some(slot_name)) = (root.file_name(), slot.file_name()) else {
        return false;
    };
    slot.parent() == root.parent()
        && slot_name
            .to_string_lossy()
            .starts_with(&format!("{}.discard-", name.to_string_lossy()))
}

/// Delete an identified tree: record the move in the journal, move the tree
/// to a fresh slot in one rename (so its identity never depends on a
/// half-deleted stamp), delete it there, then clear the record. An
/// interruption leaves a slot the journal names, which recovery removes; no
/// path the journal does not name is ever deleted.
fn discard_tree(journal: &mut Journal, path: &Path) -> Result<(), String> {
    let slot = new_discard_slot(&journal.root);
    journal.discarding = Some(Discard {
        from: path.to_path_buf(),
        to: slot.clone(),
    });
    write_journal(journal)?;
    rename(path, &slot)?;
    std::fs::remove_dir_all(&slot)
        .map_err(|e| format!("could not remove {}: {e}", slot.display()))?;
    journal.discarding = None;
    write_journal(journal)
}

/// Finish a deletion the journal recorded, if a run stopped part way.
fn settle_discard(journal: &mut Journal) -> Result<(), String> {
    let Some(discard) = journal.discarding.clone() else {
        return Ok(());
    };
    if !is_discard_slot(&journal.root, &discard.to) {
        return Err(refuse(
            journal,
            "the journal names a deletion slot LocalBox does not own",
        ));
    }
    // Only the engine tree or the kept original is ever deleted.
    if discard.from != journal.root && discard.from != journal.previous {
        return Err(refuse(
            journal,
            "the journal names a deletion of a tree it does not own",
        ));
    }
    // Both present: the recorded move has not been shown to have happened.
    if std::fs::symlink_metadata(&discard.from).is_ok()
        && std::fs::symlink_metadata(&discard.to).is_ok()
    {
        return Err(refuse(
            journal,
            "both the tree being deleted and its deletion slot exist",
        ));
    }
    match std::fs::remove_dir_all(&discard.to) {
        Ok(()) => {}
        // Stopped before the move: the tree is still at its place and the
        // ordinary checks decide what happens to it.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("could not remove {}: {e}", discard.to.display())),
    }
    journal.discarding = None;
    write_journal(journal)
}

fn rename(from: &Path, to: &Path) -> Result<(), String> {
    // A server that was just stopped can hold its files open for a moment on
    // Windows, so a refused move is retried briefly before it is an error.
    let mut attempts = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && attempts < 10 => {
                attempts += 1;
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            Err(e) => {
                return Err(format!(
                    "could not move {} to {}: {e}",
                    from.display(),
                    to.display()
                ))
            }
        }
    }
}

/// What recovery did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    /// No journal: nothing was interrupted.
    Clean,
    /// An unfinished run was undone: the original tree and settings are back.
    RestoredOriginal,
    /// A verified replacement's cleanup was finished.
    FinishedCommit,
}

fn refuse(journal: &Journal, why: &str) -> String {
    format!(
        "an interrupted backend fallback left a state LocalBox cannot settle safely ({why}). \
         Nothing was changed. Engine: {}; kept original: {}; journal: {}. Decide which \
         tree to keep (its .build-stamp names the build), make it {}, remove the other and \
         the journal, then run the command again",
        journal.root.display(),
        journal.previous.display(),
        journal_path(&journal.root).display(),
        journal.root.display()
    )
}

/// Settle an interrupted fallback run for `root`, if one left a journal.
/// Call with the engine lock held. Never deletes a tree whose identity is
/// uncertain: an unexpected state is refused with the manual steps.
///
/// # Errors
/// A plain message when the state is ambiguous or an I/O step fails.
pub fn recover(root: &Path, settings_path: &Path) -> Result<Recovery, String> {
    let Some(mut journal) = read_journal(root)? else {
        return Ok(Recovery::Clean);
    };
    // A journal is obeyed only for the engine it sits beside.
    if journal.root != root || journal.previous != previous_path(root) {
        return Err(refuse(
            &journal,
            "the journal describes a different engine folder",
        ));
    }
    settle_discard(&mut journal)?;
    let root_stamp = Stamp::read(root);
    let previous_exists = std::fs::symlink_metadata(&journal.previous).is_ok();
    match journal.phase {
        Phase::Pending => {
            if previous_exists {
                if Stamp::read(&journal.previous).as_ref() != Some(&journal.original) {
                    return Err(refuse(
                        &journal,
                        "the kept tree is not the recorded original",
                    ));
                }
                if std::fs::symlink_metadata(root).is_ok() {
                    let is_candidate = root_stamp.is_some() && root_stamp == journal.candidate;
                    if !is_candidate {
                        return Err(refuse(
                            &journal,
                            "the engine tree is neither absent nor the recorded candidate",
                        ));
                    }
                    discard_tree(&mut journal, root)?;
                }
                rename(&journal.previous, root)?;
            } else if root_stamp.as_ref() != Some(&journal.original) {
                return Err(refuse(
                    &journal,
                    "the kept tree is missing and the engine tree is not the recorded original",
                ));
            }
            let current = read_settings(settings_path)?;
            write_settings(
                settings_path,
                &restore_snapshot(&current, &journal.settings),
            )?;
            remove_journal(root)?;
            Ok(Recovery::RestoredOriginal)
        }
        Phase::Committed => {
            if root_stamp.is_none() || root_stamp != journal.candidate {
                return Err(refuse(
                    &journal,
                    "the engine tree is not the verified replacement",
                ));
            }
            if previous_exists {
                if Stamp::read(&journal.previous).as_ref() != Some(&journal.original) {
                    return Err(refuse(
                        &journal,
                        "the kept tree is not the recorded original",
                    ));
                }
                {
                    let previous = journal.previous.clone();
                    discard_tree(&mut journal, &previous)?
                };
            }
            remove_journal(root)?;
            Ok(Recovery::FinishedCommit)
        }
    }
}

/// The identity of the NVIDIA driver from an `nvidia-smi` banner: its
/// driver version, else its CUDA version. `None` with no NVIDIA driver.
#[must_use]
pub fn driver_fingerprint(banner: &str) -> Option<String> {
    ["Driver Version:", "CUDA Version:"]
        .into_iter()
        .find_map(|label| {
            let start = banner.find(label)? + label.len();
            let token = banner[start..].split_whitespace().next()?;
            (!token.is_empty() && token.chars().next()?.is_ascii_digit()).then(|| token.to_string())
        })
}

/// The fallback record a passed fallback leaves in settings.json.
#[must_use]
pub fn fallback_record(
    stamp: &Stamp,
    failed: &str,
    driver: Option<&str>,
    model: &str,
) -> serde_json::Value {
    serde_json::json!({
        "variant": stamp.variant,
        "tag": stamp.tag,
        "from": failed,
        "driver": driver,
        "model": model,
    })
}

/// What a fallback record means for the next update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FallbackStanding {
    /// No fallback is recorded.
    None,
    /// The driver is the one the fallback was decided on: keep that build.
    Holds(Variant),
    /// The driver changed since: try the build the host would pick again.
    DriverChanged {
        /// The build the fallback moved away from (e.g. `cuda-13.5`).
        from: String,
        /// The model the fallback was verified with.
        model: String,
        recorded: Option<String>,
        now: Option<String>,
    },
}

/// Read the fallback record in `settings` against the driver `now`.
#[must_use]
pub fn fallback_standing(settings: &Settings, now: Option<&str>) -> FallbackStanding {
    let Some(record) = settings.get(FALLBACK_SETTING).and_then(|v| v.as_object()) else {
        return FallbackStanding::None;
    };
    let text = |key: &str| record.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let recorded = text("driver");
    let Some(variant) = text("variant").as_deref().and_then(Variant::parse) else {
        return FallbackStanding::None;
    };
    if recorded.as_deref() == now {
        return FallbackStanding::Holds(variant);
    }
    FallbackStanding::DriverChanged {
        from: text("from").unwrap_or_default(),
        model: text("model").unwrap_or_default(),
        recorded,
        now: now.map(str::to_string),
    }
}

/// What a failed load or reply says about its cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCause {
    /// The model (at this quant and context) does not fit: another engine
    /// build would not help, and switching would slow every other model.
    OutOfMemory,
    /// The engine build itself cannot run here: a driver/toolkit mismatch, a
    /// missing device or kernel, or a crash while starting the GPU backend.
    Backend,
    /// Nothing in the evidence points either way.
    Unknown,
}

/// Classify a failure from the server log and the launch error text. Memory
/// evidence wins over everything else, because an allocation failure inside
/// the GPU backend also mentions that backend. Only [`FailureCause::Backend`]
/// justifies replacing the engine build, which every model shares.
#[must_use]
pub fn classify_failure(server_log: &str, error: &str) -> FailureCause {
    const MEMORY: [&str; 9] = [
        "out of memory",
        "outofdevicememory",
        "erroroutofdevicememory",
        "cudamalloc failed",
        "failed to allocate",
        "unable to allocate",
        "insufficient memory",
        "not enough memory",
        "failed to load model",
    ];
    const BACKEND: [&str; 12] = [
        "the provided ptx was compiled with an unsupported toolchain",
        "cuda driver version is insufficient",
        "no cuda-capable device",
        "no kernel image is available",
        "invalid device function",
        "ggml_cuda_init: failed",
        "cuda error",
        "vulkan error",
        "vk::",
        "illegal instruction",
        "0xc000001d",
        "0xc0000135",
    ];
    let text = format!("{server_log}\n{error}").to_ascii_lowercase();
    // "failed to load model" with no other evidence is usually a bad or
    // oversized file, never a reason to swap builds; but it must not mask a
    // driver error printed before it.
    let memory_hit = MEMORY
        .iter()
        .filter(|sign| **sign != "failed to load model")
        .any(|sign| text.contains(sign));
    if memory_hit {
        return FailureCause::OutOfMemory;
    }
    if BACKEND.iter().any(|sign| text.contains(sign)) {
        return FailureCause::Backend;
    }
    FailureCause::Unknown
}

/// The builds to try after `failed` (a stamp variant such as `cuda-13.5`),
/// in order. macOS's Metal build and the CPU build have nothing below them.
#[must_use]
pub fn rungs_after(failed: &str) -> Vec<Variant> {
    match Variant::parse(failed) {
        Some(Variant::Cuda) => vec![Variant::Vulkan, Variant::Cpu],
        Some(Variant::Vulkan) => vec![Variant::Cpu],
        Some(Variant::Cpu) | None => Vec::new(),
    }
}

/// One installable candidate build.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub stamp: Stamp,
    pub assets: Vec<crate::update::Asset>,
}

/// The effects a fallback run needs, so the sequence itself can be tested
/// without a network, a GPU or a model.
pub trait FallbackHost {
    /// The candidate build for `variant`, or why there is none.
    ///
    /// # Errors
    /// A plain message (e.g. no Vulkan build for this platform).
    fn resolve(&mut self, variant: Variant) -> Result<Candidate, String>;
    /// Install `candidate` at `root` (which is absent); the asset pins.
    ///
    /// # Errors
    /// A plain message when the install fails.
    fn install(
        &mut self,
        candidate: &Candidate,
        root: &Path,
    ) -> Result<Vec<(String, String)>, String>;
    /// Prove the build now at the root can load the model and answer.
    ///
    /// # Errors
    /// Why it could not, and whether that implicates the build.
    fn probe(&mut self) -> Result<(), AttemptFailure>;
    /// The settings a passed candidate leaves, from the current ones: its
    /// pins, the fallback record and the verified-backend receipt, written in
    /// one step before the run commits, so the tree and its records never
    /// disagree.
    fn record(&mut self, current: &Settings, stamp: &Stamp, pins: &[(String, String)]) -> Settings;
    /// A line for the user.
    fn say(&mut self, line: &str);
}

/// Why a candidate build did not pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptFailure {
    /// The build cannot run here: the next build is worth trying.
    Backend(String),
    /// Not the build's fault (memory, an unclear failure, a refusal): stop
    /// and put the original back, since another build would not help.
    Stop(String),
}

/// A candidate that passed, with what the caller records about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackSuccess {
    pub stamp: Stamp,
    /// Each failed attempt before it: variant and reason.
    pub failures: Vec<(String, String)>,
}

/// The steps of one run, public so tests can stop between any two of them.
pub mod steps {
    use super::{
        previous_path, read_settings, rename, snapshot, write_journal, Journal, Phase, Stamp,
    };
    use std::path::Path;

    /// Record the original tree and settings before anything moves.
    ///
    /// # Errors
    /// A plain message when the root has no complete stamp or I/O fails.
    pub fn begin(root: &Path, settings_path: &Path) -> Result<Journal, String> {
        let original = Stamp::read(root).ok_or_else(|| {
            format!(
                "the engine at {} has no build stamp, so it cannot be kept safely; \
                 run `localbox update --mode native` first",
                root.display()
            )
        })?;
        let journal = Journal {
            phase: Phase::Pending,
            root: root.to_path_buf(),
            previous: previous_path(root),
            original,
            candidate: None,
            settings: snapshot(&read_settings(settings_path)?),
            discarding: None,
        };
        write_journal(&journal)?;
        Ok(journal)
    }

    /// Move the original tree aside, where it stays for the whole run.
    ///
    /// # Errors
    /// A plain message when the move fails.
    pub fn set_aside(journal: &Journal) -> Result<(), String> {
        rename(&journal.root, &journal.previous)
    }

    /// Name the candidate about to be installed, before it is installed.
    ///
    /// # Errors
    /// A plain message when the journal cannot be written.
    pub fn announce(journal: &mut Journal, candidate: &Stamp) -> Result<(), String> {
        journal.candidate = Some(candidate.clone());
        write_journal(journal)
    }

    /// Mark the passed candidate as the record (its settings already written).
    ///
    /// # Errors
    /// A plain message when the journal cannot be written.
    pub fn commit(journal: &mut Journal) -> Result<(), String> {
        journal.phase = Phase::Committed;
        write_journal(journal)
    }

    /// Whether the tree at the root is the announced candidate.
    #[must_use]
    pub fn root_is_candidate(journal: &Journal) -> bool {
        let stamp = Stamp::read(&journal.root);
        stamp.is_some() && stamp == journal.candidate
    }

    /// The settings file content at this point, for tests.
    ///
    /// # Errors
    /// A plain message when it cannot be read.
    pub fn settings(settings_path: &Path) -> Result<super::Settings, String> {
        read_settings(settings_path)
    }
}

/// Run the fallback ladder for the failed build at `root`. Call with the
/// engine lock held and after [`recover`]. On success the passed build is at
/// `root` and `record` has written its settings; on failure the original
/// tree and settings are back.
///
/// # Errors
/// A plain message listing each attempt, after the original was restored.
pub fn run(
    root: &Path,
    settings_path: &Path,
    rungs: &[Variant],
    host: &mut dyn FallbackHost,
) -> Result<FallbackSuccess, String> {
    let mut journal = steps::begin(root, settings_path)?;
    steps::set_aside(&journal)?;
    let mut failures: Vec<(String, String)> = Vec::new();
    for variant in rungs {
        let candidate = match host.resolve(*variant) {
            Ok(candidate) => candidate,
            Err(why) => {
                host.say(&format!("No {} build to try: {why}", variant.as_str()));
                failures.push((variant.as_str().to_string(), why));
                continue;
            }
        };
        host.say(&format!(
            "Trying the {} build ({}) …",
            candidate.stamp.variant, candidate.stamp.tag
        ));
        steps::announce(&mut journal, &candidate.stamp)?;
        let pins = match host.install(&candidate, root) {
            Ok(pins) => pins,
            Err(why) => {
                failures.push((candidate.stamp.variant.clone(), why));
                if steps::root_is_candidate(&journal) {
                    discard_tree(&mut journal, root)?;
                }
                continue;
            }
        };
        if !steps::root_is_candidate(&journal) {
            return Err(refuse(
                &journal,
                "the installed candidate does not carry its stamp",
            ));
        }
        match host.probe() {
            Ok(()) => {
                let current = read_settings(settings_path)?;
                write_settings(
                    settings_path,
                    &host.record(&current, &candidate.stamp, &pins),
                )?;
                steps::commit(&mut journal)?;
                {
                    let previous = journal.previous.clone();
                    discard_tree(&mut journal, &previous)?
                };
                remove_journal(root)?;
                return Ok(FallbackSuccess {
                    stamp: candidate.stamp,
                    failures,
                });
            }
            Err(AttemptFailure::Backend(why)) => {
                host.say(&format!(
                    "The {} build failed too: {why}",
                    candidate.stamp.variant
                ));
                failures.push((candidate.stamp.variant.clone(), why));
                discard_tree(&mut journal, root)?;
            }
            Err(AttemptFailure::Stop(why)) => {
                host.say(&format!(
                    "The {} build stopped for a reason another build would not fix: {why}",
                    candidate.stamp.variant
                ));
                failures.push((candidate.stamp.variant.clone(), why));
                discard_tree(&mut journal, root)?;
                break;
            }
        }
    }
    rename(&journal.previous, root)?;
    let current = read_settings(settings_path)?;
    write_settings(
        settings_path,
        &restore_snapshot(&current, &journal.settings),
    )?;
    remove_journal(root)?;
    let tried = failures
        .iter()
        .map(|(variant, why)| format!("{variant}: {why}"))
        .collect::<Vec<_>>()
        .join("; ");
    Err(format!(
        "no other build could run this model ({tried}). The original {} build was restored",
        journal.original.variant
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::update::write_stamp;

    fn tree(root: &Path, tag: &str, variant: &str) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("llama-server"), b"bin").unwrap();
        write_stamp(root, tag, variant).unwrap();
    }

    fn stamp(tag: &str, variant: &str) -> Stamp {
        Stamp {
            tag: tag.into(),
            variant: variant.into(),
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        settings: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("llama-cpp");
        tree(&root, "b100", "cuda-13.5");
        let settings = dir.path().join("settings.json");
        std::fs::write(
            &settings,
            r#"{"LlamaCppPinnedTag":"b100","LlamaCppDownloadPins":{"a.zip":"aa"},"VRAMGB":4,"LlamaCppVerifiedBackend":{"tag":"b100","variant":"cuda-13.5"}}"#,
        )
        .unwrap();
        Fixture {
            _dir: dir,
            root,
            settings,
        }
    }

    fn settings_of(f: &Fixture) -> Settings {
        read_settings(&f.settings).unwrap()
    }

    fn candidate_settings(f: &Fixture) {
        let mut s = settings_of(f);
        s.insert("LlamaCppPinnedTag".into(), "b200".into());
        s.insert(
            FALLBACK_SETTING.into(),
            fallback_record(
                &stamp("b200", "vulkan"),
                "cuda-13.5",
                Some("591.74"),
                "qwen",
            ),
        );
        s.insert(
            VERIFIED_BACKEND_SETTING.into(),
            serde_json::json!({"tag": "b200", "variant": "vulkan"}),
        );
        write_settings(&f.settings, &s).unwrap();
    }

    fn assert_original(f: &Fixture) {
        assert_eq!(Stamp::read(&f.root), Some(stamp("b100", "cuda-13.5")));
        assert!(!previous_path(&f.root).exists());
        assert!(!journal_path(&f.root).exists());
        let s = settings_of(f);
        assert_eq!(s.get("LlamaCppPinnedTag").unwrap(), "b100");
        assert!(s.get(FALLBACK_SETTING).is_none());
        assert!(s.get(NATIVE_VARIANT_SETTING).is_none());
        assert_eq!(
            s[VERIFIED_BACKEND_SETTING]["variant"], "cuda-13.5",
            "the receipt goes back with the tree it describes"
        );
        assert_eq!(s.get("VRAMGB").unwrap(), 4, "unrelated keys survive");
    }

    fn assert_candidate(f: &Fixture) {
        assert_eq!(Stamp::read(&f.root), Some(stamp("b200", "vulkan")));
        assert!(!previous_path(&f.root).exists());
        assert!(!journal_path(&f.root).exists());
        let s = settings_of(f);
        assert_eq!(s[FALLBACK_SETTING]["variant"], "vulkan");
        assert_eq!(
            s[VERIFIED_BACKEND_SETTING]["variant"], "vulkan",
            "the receipt is written with the tree before the commit"
        );
        assert!(
            s.get(NATIVE_VARIANT_SETTING).is_none(),
            "a fallback is never recorded as the user's own choice"
        );
    }

    #[test]
    fn no_journal_is_clean_and_touches_nothing() {
        let f = fixture();
        assert_eq!(recover(&f.root, &f.settings).unwrap(), Recovery::Clean);
        assert_original(&f);
    }

    #[test]
    fn interrupted_after_the_journal_before_anything_moved_keeps_the_root() {
        let f = fixture();
        steps::begin(&f.root, &f.settings).unwrap();
        assert_eq!(
            recover(&f.root, &f.settings).unwrap(),
            Recovery::RestoredOriginal
        );
        assert_original(&f);
    }

    #[test]
    fn interrupted_after_setting_aside_restores_the_original() {
        let f = fixture();
        let j = steps::begin(&f.root, &f.settings).unwrap();
        steps::set_aside(&j).unwrap();
        assert!(!f.root.exists());
        assert_eq!(
            recover(&f.root, &f.settings).unwrap(),
            Recovery::RestoredOriginal
        );
        assert_original(&f);
    }

    #[test]
    fn interrupted_after_the_candidate_installed_removes_only_that_candidate() {
        let f = fixture();
        let mut j = steps::begin(&f.root, &f.settings).unwrap();
        steps::set_aside(&j).unwrap();
        steps::announce(&mut j, &stamp("b200", "vulkan")).unwrap();
        tree(&f.root, "b200", "vulkan");
        assert_eq!(
            recover(&f.root, &f.settings).unwrap(),
            Recovery::RestoredOriginal
        );
        assert_original(&f);
    }

    #[test]
    fn interrupted_after_candidate_settings_restores_tree_and_settings_together() {
        let f = fixture();
        let mut j = steps::begin(&f.root, &f.settings).unwrap();
        steps::set_aside(&j).unwrap();
        steps::announce(&mut j, &stamp("b200", "vulkan")).unwrap();
        tree(&f.root, "b200", "vulkan");
        candidate_settings(&f);
        assert_eq!(
            recover(&f.root, &f.settings).unwrap(),
            Recovery::RestoredOriginal
        );
        assert_original(&f);
    }

    #[test]
    fn interrupted_after_the_commit_finishes_cleanup_and_keeps_the_replacement() {
        let f = fixture();
        let mut j = steps::begin(&f.root, &f.settings).unwrap();
        steps::set_aside(&j).unwrap();
        steps::announce(&mut j, &stamp("b200", "vulkan")).unwrap();
        tree(&f.root, "b200", "vulkan");
        candidate_settings(&f);
        steps::commit(&mut j).unwrap();
        assert_eq!(
            recover(&f.root, &f.settings).unwrap(),
            Recovery::FinishedCommit
        );
        assert_candidate(&f);
    }

    #[test]
    fn interrupted_during_commit_cleanup_after_previous_was_removed_finishes() {
        let f = fixture();
        let mut j = steps::begin(&f.root, &f.settings).unwrap();
        steps::set_aside(&j).unwrap();
        steps::announce(&mut j, &stamp("b200", "vulkan")).unwrap();
        tree(&f.root, "b200", "vulkan");
        candidate_settings(&f);
        steps::commit(&mut j).unwrap();
        std::fs::remove_dir_all(&j.previous).unwrap();
        assert_eq!(
            recover(&f.root, &f.settings).unwrap(),
            Recovery::FinishedCommit
        );
        assert_candidate(&f);
    }

    #[test]
    fn a_rollback_finished_before_the_journal_was_removed_settles_without_deleting() {
        let f = fixture();
        let mut j = steps::begin(&f.root, &f.settings).unwrap();
        steps::set_aside(&j).unwrap();
        steps::announce(&mut j, &stamp("b200", "vulkan")).unwrap();
        std::fs::rename(&j.previous, &f.root).unwrap();
        assert_eq!(
            recover(&f.root, &f.settings).unwrap(),
            Recovery::RestoredOriginal
        );
        assert_original(&f);
    }

    /// Stop a deletion of `path` after it was recorded and moved, with part
    /// of the tree (its stamp included) already gone.
    fn stop_mid_discard(j: &mut Journal, path: &Path) -> PathBuf {
        let slot = new_discard_slot(&j.root);
        j.discarding = Some(Discard {
            from: path.to_path_buf(),
            to: slot.clone(),
        });
        write_journal(j).unwrap();
        std::fs::rename(path, &slot).unwrap();
        std::fs::remove_file(slot.join(".build-stamp")).unwrap();
        slot
    }

    #[test]
    fn a_candidate_caught_half_deleted_does_not_block_restoring_the_original() {
        let f = fixture();
        let mut j = steps::begin(&f.root, &f.settings).unwrap();
        steps::set_aside(&j).unwrap();
        steps::announce(&mut j, &stamp("b200", "vulkan")).unwrap();
        tree(&f.root, "b200", "vulkan");
        let root = f.root.clone();
        let slot = stop_mid_discard(&mut j, &root);
        assert_eq!(
            recover(&f.root, &f.settings).unwrap(),
            Recovery::RestoredOriginal
        );
        assert_original(&f);
        assert!(!slot.exists());
    }

    #[test]
    fn an_original_caught_half_deleted_after_the_commit_finishes_cleanup() {
        let f = fixture();
        let mut j = steps::begin(&f.root, &f.settings).unwrap();
        steps::set_aside(&j).unwrap();
        steps::announce(&mut j, &stamp("b200", "vulkan")).unwrap();
        tree(&f.root, "b200", "vulkan");
        candidate_settings(&f);
        steps::commit(&mut j).unwrap();
        let previous = j.previous.clone();
        let slot = stop_mid_discard(&mut j, &previous);
        assert_eq!(
            recover(&f.root, &f.settings).unwrap(),
            Recovery::FinishedCommit
        );
        assert_candidate(&f);
        assert!(!slot.exists());
    }

    #[test]
    fn a_deletion_recorded_but_not_yet_moved_leaves_the_tree_to_the_normal_checks() {
        let f = fixture();
        let mut j = steps::begin(&f.root, &f.settings).unwrap();
        steps::set_aside(&j).unwrap();
        steps::announce(&mut j, &stamp("b200", "vulkan")).unwrap();
        tree(&f.root, "b200", "vulkan");
        j.discarding = Some(Discard {
            from: f.root.clone(),
            to: new_discard_slot(&f.root),
        });
        write_journal(&j).unwrap();
        assert_eq!(
            recover(&f.root, &f.settings).unwrap(),
            Recovery::RestoredOriginal
        );
        assert_original(&f);
    }

    #[test]
    fn a_deletion_whose_source_and_slot_both_exist_is_refused() {
        let f = fixture();
        let mut j = steps::begin(&f.root, &f.settings).unwrap();
        steps::set_aside(&j).unwrap();
        steps::announce(&mut j, &stamp("b200", "vulkan")).unwrap();
        tree(&f.root, "b200", "vulkan");
        let slot = new_discard_slot(&f.root);
        tree(&slot, "b999", "cpu");
        j.discarding = Some(Discard {
            from: f.root.clone(),
            to: slot.clone(),
        });
        write_journal(&j).unwrap();
        assert!(recover(&f.root, &f.settings).is_err());
        assert_eq!(Stamp::read(&f.root), Some(stamp("b200", "vulkan")));
        assert_eq!(Stamp::read(&slot), Some(stamp("b999", "cpu")));
        assert!(j.previous.exists());
    }

    #[test]
    fn a_journal_for_another_engine_folder_is_refused_and_deletes_nothing() {
        let f = fixture();
        let other_root = f.root.with_file_name("elsewhere");
        let other_slot = new_discard_slot(&other_root);
        tree(&other_slot, "b1", "cpu");
        let journal = Journal {
            phase: Phase::Pending,
            root: other_root.clone(),
            previous: previous_path(&other_root),
            original: stamp("b1", "cpu"),
            candidate: None,
            settings: Settings::new(),
            discarding: Some(Discard {
                from: other_root.clone(),
                to: other_slot.clone(),
            }),
        };
        let bytes = serde_json::to_vec_pretty(&journal).unwrap();
        std::fs::write(journal_path(&f.root), bytes).unwrap();
        assert!(recover(&f.root, &f.settings).is_err());
        assert!(
            other_slot.join(".build-stamp").is_file(),
            "the other tree survives"
        );
        assert_eq!(Stamp::read(&f.root), Some(stamp("b100", "cuda-13.5")));
    }

    #[test]
    fn folders_the_journal_does_not_name_are_never_deleted() {
        let f = fixture();
        let unrelated = f.root.with_file_name("llama-cpp.discard");
        let backup = f.root.with_file_name("llama-cpp.discard-mine");
        for dir in [&unrelated, &backup] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("keep.txt"), b"mine").unwrap();
        }
        // A clean recovery and a whole run leave them alone.
        assert_eq!(recover(&f.root, &f.settings).unwrap(), Recovery::Clean);
        let mut h = host(vec![(
            Variant::Vulkan,
            Err(AttemptFailure::Backend("x".into())),
        )]);
        run(&f.root, &f.settings, &rungs_after("cuda-13.5"), &mut h).unwrap();
        // An interrupted run whose journal names a slot it does not own is
        // refused, not obeyed.
        let mut j = steps::begin(&f.root, &f.settings).unwrap();
        j.discarding = Some(Discard {
            from: f.root.clone(),
            to: unrelated.clone(),
        });
        write_journal(&j).unwrap();
        assert!(recover(&f.root, &f.settings).is_err());
        for dir in [&unrelated, &backup] {
            assert!(dir.join("keep.txt").is_file(), "{} survives", dir.display());
        }
    }

    #[test]
    fn an_unknown_tree_at_the_root_is_refused_and_nothing_is_deleted() {
        let f = fixture();
        let mut j = steps::begin(&f.root, &f.settings).unwrap();
        steps::set_aside(&j).unwrap();
        steps::announce(&mut j, &stamp("b200", "vulkan")).unwrap();
        tree(&f.root, "b300", "cpu");
        let err = recover(&f.root, &f.settings).unwrap_err();
        assert!(err.contains("Nothing was changed"), "{err}");
        assert_eq!(Stamp::read(&f.root), Some(stamp("b300", "cpu")));
        assert_eq!(Stamp::read(&j.previous), Some(stamp("b100", "cuda-13.5")));
        assert!(journal_path(&f.root).exists());
    }

    #[test]
    fn a_root_without_a_stamp_is_uncertain_and_refused() {
        let f = fixture();
        let mut j = steps::begin(&f.root, &f.settings).unwrap();
        steps::set_aside(&j).unwrap();
        steps::announce(&mut j, &stamp("b200", "vulkan")).unwrap();
        std::fs::create_dir_all(&f.root).unwrap();
        assert!(recover(&f.root, &f.settings).is_err());
        assert!(f.root.exists() && j.previous.exists());
    }

    #[test]
    fn a_kept_tree_that_is_not_the_original_is_refused() {
        let f = fixture();
        let j = steps::begin(&f.root, &f.settings).unwrap();
        steps::set_aside(&j).unwrap();
        write_stamp(&j.previous, "b999", "cpu").unwrap();
        assert!(recover(&f.root, &f.settings).is_err());
        assert!(j.previous.exists());
    }

    #[test]
    fn a_committed_journal_whose_root_is_not_the_candidate_is_refused() {
        let f = fixture();
        let mut j = steps::begin(&f.root, &f.settings).unwrap();
        steps::set_aside(&j).unwrap();
        steps::announce(&mut j, &stamp("b200", "vulkan")).unwrap();
        steps::commit(&mut j).unwrap();
        assert!(recover(&f.root, &f.settings).is_err());
        assert!(
            j.previous.exists(),
            "the original is never deleted on doubt"
        );
    }

    struct FakeHost {
        outcomes: Vec<(Variant, Result<(), AttemptFailure>)>,
        unavailable: Vec<Variant>,
        current: Option<Variant>,
        said: Vec<String>,
    }

    impl FallbackHost for FakeHost {
        fn resolve(&mut self, variant: Variant) -> Result<Candidate, String> {
            if self.unavailable.contains(&variant) {
                return Err("not published for this platform".into());
            }
            self.current = Some(variant);
            Ok(Candidate {
                stamp: stamp("b200", variant.as_str()),
                assets: Vec::new(),
            })
        }
        fn install(
            &mut self,
            candidate: &Candidate,
            root: &Path,
        ) -> Result<Vec<(String, String)>, String> {
            tree(root, &candidate.stamp.tag, &candidate.stamp.variant);
            Ok(vec![(
                format!("{}.zip", candidate.stamp.variant),
                "bb".into(),
            )])
        }
        fn probe(&mut self) -> Result<(), AttemptFailure> {
            let current = self.current.unwrap();
            self.outcomes
                .iter()
                .find(|(v, _)| *v == current)
                .map(|(_, r)| r.clone())
                .unwrap_or(Ok(()))
        }
        fn record(
            &mut self,
            current: &Settings,
            stamp: &Stamp,
            pins: &[(String, String)],
        ) -> Settings {
            record(current, stamp, pins)
        }
        fn say(&mut self, line: &str) {
            self.said.push(line.to_string());
        }
    }

    fn record(current: &Settings, stamp: &Stamp, _pins: &[(String, String)]) -> Settings {
        let mut next = current.clone();
        next.insert("LlamaCppPinnedTag".into(), stamp.tag.clone().into());
        next.insert(
            FALLBACK_SETTING.into(),
            fallback_record(stamp, "cuda-13.5", Some("591.74"), "qwen"),
        );
        next.insert(
            VERIFIED_BACKEND_SETTING.into(),
            serde_json::json!({"tag": stamp.tag, "variant": stamp.variant}),
        );
        next
    }

    fn host(outcomes: Vec<(Variant, Result<(), AttemptFailure>)>) -> FakeHost {
        FakeHost {
            outcomes,
            unavailable: Vec::new(),
            current: None,
            said: Vec::new(),
        }
    }

    #[test]
    fn vulkan_passing_replaces_cuda_and_discards_the_original() {
        let f = fixture();
        let mut h = host(vec![]);
        let ok = run(&f.root, &f.settings, &rungs_after("cuda-13.5"), &mut h).unwrap();
        assert_eq!(ok.stamp, stamp("b200", "vulkan"));
        assert_candidate(&f);
    }

    #[test]
    fn cpu_is_tried_only_after_vulkan_fails_for_a_backend_reason() {
        let f = fixture();
        let mut h = host(vec![(
            Variant::Vulkan,
            Err(AttemptFailure::Backend("vk error".into())),
        )]);
        let ok = run(&f.root, &f.settings, &rungs_after("cuda-13.5"), &mut h).unwrap();
        assert_eq!(ok.stamp, stamp("b200", "cpu"));
        assert_eq!(ok.failures.len(), 1);
    }

    #[test]
    fn a_non_backend_failure_stops_the_ladder_and_restores_the_original() {
        let f = fixture();
        let mut h = host(vec![(
            Variant::Vulkan,
            Err(AttemptFailure::Stop("out of memory".into())),
        )]);
        let err = run(&f.root, &f.settings, &rungs_after("cuda-13.5"), &mut h).unwrap_err();
        assert!(err.contains("restored"), "{err}");
        assert!(
            h.said.iter().all(|l| !l.contains("cpu build")),
            "CPU was never tried: {:?}",
            h.said
        );
        assert_original(&f);
    }

    #[test]
    fn every_build_failing_restores_the_original_tree_and_settings() {
        let f = fixture();
        let mut h = host(vec![
            (Variant::Vulkan, Err(AttemptFailure::Backend("a".into()))),
            (Variant::Cpu, Err(AttemptFailure::Backend("b".into()))),
        ]);
        assert!(run(&f.root, &f.settings, &rungs_after("cuda-13.5"), &mut h).is_err());
        assert_original(&f);
    }

    #[test]
    fn a_platform_without_vulkan_goes_straight_to_cpu() {
        let f = fixture();
        let mut h = host(vec![]);
        h.unavailable.push(Variant::Vulkan);
        let ok = run(&f.root, &f.settings, &rungs_after("cuda-12.4"), &mut h).unwrap();
        assert_eq!(ok.stamp.variant, "cpu");
    }

    #[test]
    fn the_ladder_follows_the_failed_build() {
        assert_eq!(
            rungs_after("cuda-13.5"),
            vec![Variant::Vulkan, Variant::Cpu]
        );
        assert_eq!(rungs_after("vulkan"), vec![Variant::Cpu]);
        assert!(rungs_after("cpu").is_empty());
        assert!(rungs_after("metal").is_empty());
    }

    #[test]
    fn memory_evidence_never_blames_the_build() {
        let log = "ggml_backend_cuda_buffer_type_alloc_buffer: allocating 9000 MiB on device 0: cudaMalloc failed: out of memory\nCUDA error: out of memory";
        assert_eq!(classify_failure(log, ""), FailureCause::OutOfMemory);
        assert_eq!(
            classify_failure("ggml_vulkan: ErrorOutOfDeviceMemory", ""),
            FailureCause::OutOfMemory
        );
    }

    #[test]
    fn driver_and_kernel_evidence_blames_the_build() {
        assert_eq!(
            classify_failure(
                "CUDA error: the provided PTX was compiled with an unsupported toolchain",
                ""
            ),
            FailureCause::Backend
        );
        assert_eq!(
            classify_failure(
                "",
                "the model server exited before it was ready (exit code: 0xc000001d)"
            ),
            FailureCause::Backend
        );
        assert_eq!(
            classify_failure("no kernel image is available for execution", ""),
            FailureCause::Backend
        );
    }

    #[test]
    fn a_bare_load_failure_or_silence_is_unknown() {
        assert_eq!(
            classify_failure("llama_model_load: failed to load model", ""),
            FailureCause::Unknown
        );
        assert_eq!(
            classify_failure("", "did not become ready within 300s"),
            FailureCause::Unknown
        );
    }

    #[test]
    fn the_driver_fingerprint_is_the_driver_version() {
        let banner = "| NVIDIA-SMI 591.74                 Driver Version: 591.74         CUDA Version: 13.1     |";
        assert_eq!(driver_fingerprint(banner).as_deref(), Some("591.74"));
        assert_eq!(
            driver_fingerprint("CUDA Version: 13.1").as_deref(),
            Some("13.1")
        );
        assert_eq!(driver_fingerprint(""), None);
    }

    fn with_record(driver: Option<&str>) -> Settings {
        let mut s = Settings::new();
        s.insert(
            FALLBACK_SETTING.into(),
            fallback_record(&stamp("b200", "vulkan"), "cuda-13.5", driver, "qwen"),
        );
        s
    }

    #[test]
    fn a_fallback_holds_only_while_the_driver_is_the_same() {
        let s = with_record(Some("591.74"));
        assert_eq!(
            fallback_standing(&s, Some("591.74")),
            FallbackStanding::Holds(Variant::Vulkan)
        );
        match fallback_standing(&s, Some("592.10")) {
            FallbackStanding::DriverChanged {
                from,
                model,
                recorded,
                now,
            } => {
                assert_eq!(from, "cuda-13.5");
                assert_eq!(model, "qwen");
                assert_eq!(recorded.as_deref(), Some("591.74"));
                assert_eq!(now.as_deref(), Some("592.10"));
            }
            other => panic!("expected a driver change, got {other:?}"),
        }
        assert!(matches!(
            fallback_standing(&s, None),
            FallbackStanding::DriverChanged { .. }
        ));
        assert_eq!(
            fallback_standing(&Settings::new(), Some("591.74")),
            FallbackStanding::None
        );
    }

    #[test]
    fn snapshot_restore_puts_back_absent_keys_as_absent() {
        let mut current = Settings::new();
        current.insert(NATIVE_VARIANT_SETTING.into(), "cpu".into());
        current.insert("Other".into(), 1.into());
        let restored = restore_snapshot(&current, &Settings::new());
        assert!(restored.get(NATIVE_VARIANT_SETTING).is_none());
        assert_eq!(restored.get("Other").unwrap(), 1);
    }

    #[test]
    fn a_second_holder_of_the_engine_lock_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let inner = with_engine_lock(dir.path(), || {
            let other = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "fallback::tests::lock_probe_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("LOCALBOX_TEST_LOCK_DIR", dir.path())
                .output()
                .map_err(|e| e.to_string())?;
            Ok(String::from_utf8_lossy(&other.stdout).to_string())
        })
        .unwrap();
        assert!(inner.contains("LOCK_REFUSED"), "{inner}");
        // Released when the holder returns.
        assert!(with_engine_lock(dir.path(), || Ok(())).is_ok());
    }

    #[test]
    #[ignore = "child process for a_second_holder_of_the_engine_lock_is_refused"]
    fn lock_probe_child() {
        let Some(dir) = std::env::var_os("LOCALBOX_TEST_LOCK_DIR") else {
            return;
        };
        match with_engine_lock(Path::new(&dir), || Ok(())) {
            Err(e) if e.contains("another LocalBox command") => println!("LOCK_REFUSED"),
            other => println!("UNEXPECTED {other:?}"),
        }
    }
}
