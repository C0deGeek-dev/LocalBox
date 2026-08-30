//! One run-profile resolver for every LocalBox launch surface.
//!
//! A saved LocalBench result is preferred automatically. Callers receive a
//! typed outcome when it is absent or unusable, so they can require an
//! explicit fallback decision instead of losing a warning in child-process
//! output.

use std::io::Write as _;
use std::ops::Range;
use std::path::{Path, PathBuf};

use localx_llama_core::tuner::Profile;
use localx_llama_core::{Mode, TunerBestConfig, TunerEntry, CURRENT_TUNER_VERSION};

use crate::orchestrate::LaunchRequest;

const ADOPTED_FROM_FIELD: &str = "localbox_adopted_from_tuner_version";
const ADOPTED_AT_FIELD: &str = "localbox_adopted_at";

/// Optional constraints and ranking hints for a profile lookup.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunProfileQuery<'a> {
    /// Only entries for this quant are eligible.
    pub quant: Option<&'a str>,
    /// Only entries for this context key are eligible.
    pub context_key: Option<&'a str>,
    /// Only entries for this backend are eligible.
    pub mode: Option<Mode>,
    /// Prefer this tuning profile before score.
    pub preferred_profile: Option<Profile>,
    /// Prefer measurements closest to this VRAM size before score.
    pub vram_gb: Option<i64>,
}

/// Why the saved profile could not be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableReason {
    /// No profile file has been written.
    Missing,
    /// The file is not valid JSON for the shared tuner schema.
    Invalid,
    /// The store schema is newer or older than this LocalBox build supports.
    UnsupportedSchema,
    /// Matching entries exist, but their measurement methodology is unsupported.
    UnsupportedTunerVersion,
    /// The store is valid but has no entry satisfying the requested fields.
    NoMatch,
}

/// Provenance retained when LocalBox carries an older tune's runnable settings
/// into the current measurement generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileAdoption {
    /// Measurement generation the runnable settings came from.
    pub from_tuner_version: i64,
    /// UTC timestamp at which LocalBox adopted the settings.
    pub adopted_at: String,
}

/// A saved tune could not be adopted without risking the store.
#[derive(Debug, thiserror::Error)]
pub enum ProfileAdoptionError {
    /// The resolution did not retain an older matching entry.
    #[error("no matching superseded tune is available to adopt")]
    NoSupersededTune,
    /// The store changed after it was resolved and no longer contains that entry.
    #[error("the selected superseded tune is no longer present in the saved profile")]
    SelectedTuneChanged,
    /// The profile JSON or its shared typed contract is invalid.
    #[error("the saved profile cannot be adopted safely: {0}")]
    InvalidStore(String),
    /// A filesystem operation failed.
    #[error("could not update the saved profile: {0}")]
    Io(#[from] std::io::Error),
}

impl UnavailableReason {
    /// Stable machine-readable spelling used by the catalog JSON contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Invalid => "invalid",
            Self::UnsupportedSchema => "unsupported_schema",
            Self::UnsupportedTunerVersion => "unsupported_tuner_version",
            Self::NoMatch => "no_match",
        }
    }
}

/// The resolved saved-profile state for one model and query.
#[derive(Debug, Clone)]
pub struct RunProfileResolution {
    /// Exact source path inspected.
    pub path: PathBuf,
    /// Selected tuned entry, when one is usable.
    pub entry: Option<TunerEntry>,
    /// Best matching older entry, retained only when no current entry wins.
    pub superseded: Option<TunerEntry>,
    /// Adoption provenance for the selected current entry, when any.
    pub adoption: Option<ProfileAdoption>,
    /// Typed failure when no entry is usable.
    pub unavailable: Option<UnavailableReason>,
}

impl RunProfileResolution {
    /// Whether this resolution will use tuned settings.
    #[must_use]
    pub const fn is_tuned(&self) -> bool {
        self.entry.is_some()
    }

    /// A visible, actionable warning suitable for CLI and parent processes.
    #[must_use]
    pub fn warning(&self, key: &str) -> Option<String> {
        if let Some(entry) = &self.superseded {
            let settings = rendered_overrides(entry);
            return Some(format!(
                "This model has a saved tune measured with tuner version {}. Current version {} measures through templated /v1/chat/completions with LocalBox's settings overlay and single-session defaults, so the scores are not comparable.\n\n{} · {} · {} · {} GB VRAM\n{}\nmeasured {}\n\nRe-tuning with `localbench findbest {key}` is the accurate option. Adopting keeps these runnable settings and stops the warning, but does not re-verify the old fastest claim.",
                entry.tuner_version,
                CURRENT_TUNER_VERSION,
                entry.quant,
                entry.context_key,
                entry.mode.as_str(),
                entry.vram_gb,
                settings,
                entry.measured_at,
            ));
        }
        let reason = match self.unavailable? {
            UnavailableReason::Missing => "no saved tuned profile exists",
            UnavailableReason::Invalid => "the saved tuned profile is invalid",
            UnavailableReason::UnsupportedSchema => {
                "the saved tuned profile uses an unsupported schema"
            }
            UnavailableReason::UnsupportedTunerVersion => {
                "matching saved entries use an unsupported tuner measurement version"
            }
            UnavailableReason::NoMatch => {
                "no saved tuned entry matches the requested quant/context/mode"
            }
        };
        Some(format!(
            "Warning: {reason} for '{key}' at {}; continuing uses LocalBox defaults. Run `localbench findbest {key}` to configure tuned settings.",
            self.path.display()
        ))
    }

    /// Apply the selected entry while preserving explicitly pinned fields.
    pub fn apply_to_request(
        &self,
        request: &mut LaunchRequest,
        explicit_mode: bool,
        explicit_quant: bool,
        explicit_context: bool,
    ) {
        let Some(entry) = &self.entry else {
            return;
        };
        if !explicit_mode {
            request.mode = entry.mode;
        }
        if !explicit_quant {
            request.quant = Some(entry.quant.clone());
        }
        if !explicit_context {
            request.context_key = entry.context_key.clone();
        }
        request.params = entry.overrides.to_launch_params();
    }
}

fn rendered_overrides(entry: &TunerEntry) -> String {
    let Ok(serde_json::Value::Object(fields)) = serde_json::to_value(&entry.overrides) else {
        return "settings unavailable".to_string();
    };
    if fields.is_empty() {
        return "settings: no explicit llama-server overrides".to_string();
    }
    let values = fields
        .into_iter()
        .map(|(name, value)| {
            let value = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_string);
            format!("{name}={value}")
        })
        .collect::<Vec<_>>()
        .join(" · ");
    format!("settings: {values}")
}

fn profile_path(home: &Path, key: &str) -> PathBuf {
    home.join(".local-llm")
        .join("tuner")
        .join(format!("best-{key}.json"))
}

fn profile_rank(entry: &TunerEntry, preferred: Option<Profile>) -> u8 {
    u8::from(preferred.is_some_and(|profile| entry.profile != profile))
}

fn matches_query(entry: &TunerEntry, query: RunProfileQuery<'_>) -> bool {
    query.quant.is_none_or(|quant| entry.quant == quant)
        && query
            .context_key
            .is_none_or(|context| entry.context_key == context)
        && query.mode.is_none_or(|mode| entry.mode == mode)
}

fn ranked_matches<'a>(
    store: &'a TunerBestConfig,
    query: RunProfileQuery<'_>,
) -> Vec<(usize, &'a TunerEntry)> {
    let mut entries = store
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| matches_query(entry, query))
        .collect::<Vec<_>>();
    entries.sort_by(|(_, a), (_, b)| {
        profile_rank(a, query.preferred_profile)
            .cmp(&profile_rank(b, query.preferred_profile))
            .then_with(|| {
                query.vram_gb.map_or(std::cmp::Ordering::Equal, |vram| {
                    (a.vram_gb - vram).abs().cmp(&(b.vram_gb - vram).abs())
                })
            })
            .then_with(|| b.score.total_cmp(&a.score))
    });
    entries
}

fn select_run_profile_with_index<'a>(
    store: &'a TunerBestConfig,
    query: RunProfileQuery<'_>,
) -> Option<(usize, &'a TunerEntry)> {
    if !store.schema_supported() {
        return None;
    }
    ranked_matches(store, query)
        .into_iter()
        .find(|(_, entry)| entry.measurement_supported())
}

fn select_superseded_profile<'a>(
    store: &'a TunerBestConfig,
    query: RunProfileQuery<'_>,
) -> Option<&'a TunerEntry> {
    ranked_matches(store, query)
        .into_iter()
        .map(|(_, entry)| entry)
        .find(|entry| entry.tuner_version < CURRENT_TUNER_VERSION)
}

/// Select an entry from an already-loaded store. This is the pure seam used
/// by the file-backed resolver and by guided-plan previews.
#[must_use]
pub fn select_run_profile<'a>(
    store: &'a TunerBestConfig,
    query: RunProfileQuery<'_>,
) -> Option<&'a TunerEntry> {
    select_run_profile_with_index(store, query).map(|(_, entry)| entry)
}

fn adoption_from_value(value: &serde_json::Value) -> Option<ProfileAdoption> {
    let value = value.as_object()?;
    Some(ProfileAdoption {
        from_tuner_version: value.get(ADOPTED_FROM_FIELD)?.as_i64()?,
        adopted_at: value.get(ADOPTED_AT_FIELD)?.as_str()?.to_string(),
    })
}

/// Resolve the best saved entry using the same selection path for direct,
/// guided, and parent-process launches.
#[must_use]
pub fn resolve_run_profile(
    home: &Path,
    key: &str,
    query: RunProfileQuery<'_>,
) -> RunProfileResolution {
    let path = profile_path(home, key);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(_) => {
            return RunProfileResolution {
                path,
                entry: None,
                superseded: None,
                adoption: None,
                unavailable: Some(UnavailableReason::Missing),
            };
        }
    };
    let document: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(document) => document,
        Err(_) => {
            return RunProfileResolution {
                path,
                entry: None,
                superseded: None,
                adoption: None,
                unavailable: Some(UnavailableReason::Invalid),
            };
        }
    };
    let store: TunerBestConfig = match serde_json::from_value(document.clone()) {
        Ok(store) => store,
        Err(_) => {
            return RunProfileResolution {
                path,
                entry: None,
                superseded: None,
                adoption: None,
                unavailable: Some(UnavailableReason::Invalid),
            };
        }
    };
    if !store.schema_supported() {
        return RunProfileResolution {
            path,
            entry: None,
            superseded: None,
            adoption: None,
            unavailable: Some(UnavailableReason::UnsupportedSchema),
        };
    }
    let selected = select_run_profile_with_index(&store, query);
    let entry = selected.map(|(_, entry)| entry.clone());
    let adoption = selected.and_then(|(index, _)| {
        document
            .get("entries")?
            .as_array()?
            .get(index)
            .and_then(adoption_from_value)
    });
    let superseded = entry
        .is_none()
        .then(|| select_superseded_profile(&store, query).cloned())
        .flatten();
    let unavailable = if entry.is_some() {
        None
    } else if store
        .entries
        .iter()
        .any(|candidate| matches_query(candidate, query))
    {
        Some(UnavailableReason::UnsupportedTunerVersion)
    } else {
        Some(UnavailableReason::NoMatch)
    };
    RunProfileResolution {
        path,
        unavailable,
        entry,
        superseded,
        adoption,
    }
}

fn format_utc_timestamp(unix_seconds: u64) -> String {
    let days = i64::try_from(unix_seconds / 86_400).unwrap_or(i64::MAX);
    let seconds = unix_seconds % 86_400;
    let hour = seconds / 3_600;
    let minute = (seconds % 3_600) / 60;
    let second = seconds % 60;

    // Gregorian civil date from days since the Unix epoch. The range reached
    // by SystemTime is far inside the arithmetic bounds used here.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn current_utc_timestamp() -> Result<String, ProfileAdoptionError> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| ProfileAdoptionError::InvalidStore(error.to_string()))?
        .as_secs();
    Ok(format_utc_timestamp(seconds))
}

fn skip_json_whitespace(bytes: &[u8], mut index: usize, limit: usize) -> usize {
    while index < limit && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    index
}

fn json_string_end(bytes: &[u8], start: usize, limit: usize) -> Option<usize> {
    if start >= limit || bytes[start] != b'"' {
        return None;
    }
    let mut index = start + 1;
    while index < limit {
        match bytes[index] {
            b'\\' => index = index.checked_add(2)?,
            b'"' => return Some(index + 1),
            _ => index += 1,
        }
    }
    None
}

fn json_value_end(bytes: &[u8], start: usize, limit: usize) -> Option<usize> {
    if start >= limit {
        return None;
    }
    if bytes[start] == b'"' {
        return json_string_end(bytes, start, limit);
    }
    if matches!(bytes[start], b'{' | b'[') {
        let mut expected = Vec::new();
        let mut index = start;
        while index < limit {
            match bytes[index] {
                b'"' => index = json_string_end(bytes, index, limit)?,
                b'{' => {
                    expected.push(b'}');
                    index += 1;
                }
                b'[' => {
                    expected.push(b']');
                    index += 1;
                }
                b'}' | b']' => {
                    if expected.pop()? != bytes[index] {
                        return None;
                    }
                    index += 1;
                    if expected.is_empty() {
                        return Some(index);
                    }
                }
                _ => index += 1,
            }
        }
        return None;
    }

    let mut index = start;
    while index < limit
        && !bytes[index].is_ascii_whitespace()
        && !matches!(bytes[index], b',' | b']' | b'}')
    {
        index += 1;
    }
    (index > start).then_some(index)
}

fn json_root_span(raw: &str) -> Option<Range<usize>> {
    let bytes = raw.as_bytes();
    let start = skip_json_whitespace(bytes, 0, bytes.len());
    let end = json_value_end(bytes, start, bytes.len())?;
    (skip_json_whitespace(bytes, end, bytes.len()) == bytes.len()).then_some(start..end)
}

fn json_object_field_span(raw: &str, object: Range<usize>, wanted: &str) -> Option<Range<usize>> {
    let bytes = raw.as_bytes();
    if object.start >= object.end || bytes[object.start] != b'{' || bytes[object.end - 1] != b'}' {
        return None;
    }
    let mut index = skip_json_whitespace(bytes, object.start + 1, object.end);
    while index < object.end && bytes[index] != b'}' {
        let key_start = index;
        let key_end = json_string_end(bytes, key_start, object.end)?;
        let key: String = serde_json::from_str(&raw[key_start..key_end]).ok()?;
        index = skip_json_whitespace(bytes, key_end, object.end);
        if index >= object.end || bytes[index] != b':' {
            return None;
        }
        index = skip_json_whitespace(bytes, index + 1, object.end);
        let value_start = index;
        let value_end = json_value_end(bytes, value_start, object.end)?;
        if key == wanted {
            return Some(value_start..value_end);
        }
        index = skip_json_whitespace(bytes, value_end, object.end);
        match bytes.get(index) {
            Some(b',') => {
                index = skip_json_whitespace(bytes, index + 1, object.end);
            }
            Some(b'}') => return None,
            _ => return None,
        }
    }
    None
}

fn json_array_item_span(
    raw: &str,
    array: Range<usize>,
    wanted_index: usize,
) -> Option<Range<usize>> {
    let bytes = raw.as_bytes();
    if array.start >= array.end || bytes[array.start] != b'[' || bytes[array.end - 1] != b']' {
        return None;
    }
    let mut item_index = 0;
    let mut index = skip_json_whitespace(bytes, array.start + 1, array.end);
    while index < array.end && bytes[index] != b']' {
        let value_start = index;
        let value_end = json_value_end(bytes, value_start, array.end)?;
        if item_index == wanted_index {
            return Some(value_start..value_end);
        }
        item_index += 1;
        index = skip_json_whitespace(bytes, value_end, array.end);
        match bytes.get(index) {
            Some(b',') => index = skip_json_whitespace(bytes, index + 1, array.end),
            Some(b']') => return None,
            _ => return None,
        }
    }
    None
}

fn patched_adoption_json(
    raw: &str,
    entry_index: usize,
    from_tuner_version: i64,
    adopted_at: &str,
) -> Result<String, ProfileAdoptionError> {
    let root = json_root_span(raw).ok_or_else(|| {
        ProfileAdoptionError::InvalidStore("could not locate the root JSON object".to_string())
    })?;
    let entries = json_object_field_span(raw, root, "entries").ok_or_else(|| {
        ProfileAdoptionError::InvalidStore("could not locate the entries array".to_string())
    })?;
    let entry = json_array_item_span(raw, entries, entry_index).ok_or_else(|| {
        ProfileAdoptionError::InvalidStore("could not locate the selected entry".to_string())
    })?;
    let insert_at = entry.end.checked_sub(1).ok_or_else(|| {
        ProfileAdoptionError::InvalidStore("the selected entry is empty".to_string())
    })?;
    if raw.as_bytes().get(insert_at) != Some(&b'}') {
        return Err(ProfileAdoptionError::InvalidStore(
            "the selected entry is not an object".to_string(),
        ));
    }

    let mut replacements = vec![(
        json_object_field_span(raw, entry.clone(), "tuner_version").ok_or_else(|| {
            ProfileAdoptionError::InvalidStore(
                "the selected entry has no tuner_version".to_string(),
            )
        })?,
        CURRENT_TUNER_VERSION.to_string(),
    )];
    let mut additions = Vec::new();
    if let Some(span) = json_object_field_span(raw, entry.clone(), ADOPTED_FROM_FIELD) {
        replacements.push((span, from_tuner_version.to_string()));
    } else {
        additions.push(format!("\"{ADOPTED_FROM_FIELD}\":{from_tuner_version}"));
    }
    let adopted_at = serde_json::to_string(adopted_at)
        .map_err(|error| ProfileAdoptionError::InvalidStore(error.to_string()))?;
    if let Some(span) = json_object_field_span(raw, entry, ADOPTED_AT_FIELD) {
        replacements.push((span, adopted_at));
    } else {
        additions.push(format!("\"{ADOPTED_AT_FIELD}\":{adopted_at}"));
    }
    replacements.sort_by_key(|(span, _)| span.start);

    let insertion = if additions.is_empty() {
        String::new()
    } else {
        format!(",{}", additions.join(","))
    };
    let mut patched = String::with_capacity(raw.len() + insertion.len() + 16);
    let mut cursor = 0;
    for (span, replacement) in replacements {
        if span.start < cursor || span.end > insert_at {
            return Err(ProfileAdoptionError::InvalidStore(
                "the selected entry has overlapping fields".to_string(),
            ));
        }
        patched.push_str(&raw[cursor..span.start]);
        patched.push_str(&replacement);
        cursor = span.end;
    }
    patched.push_str(&raw[cursor..insert_at]);
    patched.push_str(&insertion);
    patched.push_str(&raw[insert_at..]);
    Ok(patched)
}

fn write_adopted_store(path: &Path, payload: &str) -> Result<(), ProfileAdoptionError> {
    let temporary = path.with_extension("json.tmp");
    let backup = path.with_extension("json.bak");

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(payload.as_bytes())?;
    file.sync_all()?;
    drop(file);

    if backup.is_file() {
        std::fs::remove_file(&backup)?;
    }
    std::fs::rename(path, &backup)?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        // Keep the `.bak` and restore a readable live copy when the final swap
        // itself fails. The original error remains the caller-visible result.
        let _ = std::fs::copy(&backup, path);
        return Err(error.into());
    }
    Ok(())
}

fn adopt_superseded_profile_at(
    resolution: &mut RunProfileResolution,
    adopted_at: &str,
) -> Result<(), ProfileAdoptionError> {
    let selected = resolution
        .superseded
        .clone()
        .ok_or(ProfileAdoptionError::NoSupersededTune)?;
    if selected.tuner_version >= CURRENT_TUNER_VERSION {
        return Err(ProfileAdoptionError::NoSupersededTune);
    }

    let raw = std::fs::read_to_string(&resolution.path)?;
    let document: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| ProfileAdoptionError::InvalidStore(error.to_string()))?;
    let store: TunerBestConfig = serde_json::from_value(document.clone())
        .map_err(|error| ProfileAdoptionError::InvalidStore(error.to_string()))?;
    if !store.schema_supported() {
        return Err(ProfileAdoptionError::InvalidStore(format!(
            "schema {} is unsupported",
            store.schema
        )));
    }

    let entries = document
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| ProfileAdoptionError::InvalidStore("entries is not an array".to_string()))?;
    let entry_index = entries
        .iter()
        .position(|candidate| {
            serde_json::from_value::<TunerEntry>((*candidate).clone())
                .is_ok_and(|candidate| candidate == selected)
        })
        .ok_or(ProfileAdoptionError::SelectedTuneChanged)?;
    let patched = patched_adoption_json(&raw, entry_index, selected.tuner_version, adopted_at)?;
    let validated_document: serde_json::Value = serde_json::from_str(&patched)
        .map_err(|error| ProfileAdoptionError::InvalidStore(error.to_string()))?;
    let validated: TunerBestConfig = serde_json::from_value(validated_document.clone())
        .map_err(|error| ProfileAdoptionError::InvalidStore(error.to_string()))?;
    if !validated.schema_supported() {
        return Err(ProfileAdoptionError::InvalidStore(format!(
            "schema {} is unsupported",
            validated.schema
        )));
    }
    let adopted_entry: TunerEntry = validated_document
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .and_then(|entries| entries.get(entry_index))
        .cloned()
        .ok_or_else(|| {
            ProfileAdoptionError::InvalidStore(
                "the adopted entry disappeared during validation".to_string(),
            )
        })
        .and_then(|entry| {
            serde_json::from_value(entry)
                .map_err(|error| ProfileAdoptionError::InvalidStore(error.to_string()))
        })?;

    write_adopted_store(&resolution.path, &patched)?;
    resolution.entry = Some(adopted_entry);
    resolution.superseded = None;
    resolution.adoption = Some(ProfileAdoption {
        from_tuner_version: selected.tuner_version,
        adopted_at: adopted_at.to_string(),
    });
    resolution.unavailable = None;
    Ok(())
}

/// Adopt the retained older tune by carrying its runnable settings into the
/// current generation, preserving explicit provenance and every unrelated JSON
/// field. The previous store remains beside the live file as `.json.bak`.
///
/// # Errors
/// Returns an error when there is no older match, the store changed or became
/// invalid, or the crash-safe write could not complete.
pub fn adopt_superseded_profile(
    resolution: &mut RunProfileResolution,
) -> Result<(), ProfileAdoptionError> {
    let adopted_at = current_utc_timestamp()?;
    adopt_superseded_profile_at(resolution, &adopted_at)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use localx_llama_core::tuner::{Overrides, PromptLength, SearchStrategy};

    fn entry(quant: &str, score: f64, profile: Profile, vram_gb: i64) -> TunerEntry {
        TunerEntry {
            quant: quant.to_string(),
            context_key: "64k".to_string(),
            context_tokens: Some(65_536),
            mode: Mode::Native,
            vram_gb,
            prompt_length: PromptLength::Short,
            profile,
            search_strategy: Some(SearchStrategy::Greedy),
            beam_width: None,
            score,
            score_unit: "tok/s".to_string(),
            pure_score: None,
            args: Vec::new(),
            overrides: Overrides::default(),
            measured_at: "2026-08-10T00:00:00Z".to_string(),
            tuner_version: CURRENT_TUNER_VERSION,
            trial_count: None,
            gpu_names: None,
            llamacpp_build: None,
        }
    }

    fn write_store(home: &Path, entries: Vec<TunerEntry>) {
        let dir = home.join(".local-llm").join("tuner");
        std::fs::create_dir_all(&dir).unwrap();
        let store = TunerBestConfig {
            schema: 1,
            key: "model".to_string(),
            vram_gb: Some(24),
            entries,
        };
        std::fs::write(
            dir.join("best-model.json"),
            serde_json::to_string(&store).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn missing_invalid_and_unsupported_are_typed() {
        let home = tempfile::tempdir().unwrap();
        let missing = resolve_run_profile(home.path(), "model", RunProfileQuery::default());
        assert_eq!(missing.unavailable, Some(UnavailableReason::Missing));
        std::fs::create_dir_all(home.path().join(".local-llm/tuner")).unwrap();
        std::fs::write(home.path().join(".local-llm/tuner/best-model.json"), "{").unwrap();
        let invalid = resolve_run_profile(home.path(), "model", RunProfileQuery::default());
        assert_eq!(invalid.unavailable, Some(UnavailableReason::Invalid));
        std::fs::write(
            home.path().join(".local-llm/tuner/best-model.json"),
            r#"{"schema":99,"key":"model","entries":[]}"#,
        )
        .unwrap();
        let unsupported = resolve_run_profile(home.path(), "model", RunProfileQuery::default());
        assert_eq!(
            unsupported.unavailable,
            Some(UnavailableReason::UnsupportedSchema)
        );
    }

    #[test]
    fn one_selector_honors_constraints_profile_vram_then_score() {
        let home = tempfile::tempdir().unwrap();
        write_store(
            home.path(),
            vec![
                entry("q4", 100.0, Profile::Pure, 48),
                entry("q4", 80.0, Profile::Balanced, 24),
                entry("q8", 200.0, Profile::Balanced, 24),
            ],
        );
        let found = resolve_run_profile(
            home.path(),
            "model",
            RunProfileQuery {
                quant: Some("q4"),
                preferred_profile: Some(Profile::Balanced),
                vram_gb: Some(24),
                ..RunProfileQuery::default()
            },
        );
        assert_eq!(found.entry.unwrap().score, 80.0);
    }

    #[test]
    fn unsupported_versions_are_distinct_and_mixed_stores_select_current() {
        let home = tempfile::tempdir().unwrap();
        let mut older = entry("q4", 1_000.0, Profile::Balanced, 24);
        older.tuner_version = CURRENT_TUNER_VERSION - 1;
        let mut newer = older.clone();
        newer.tuner_version = CURRENT_TUNER_VERSION + 1;

        write_store(home.path(), vec![older.clone(), newer]);
        let unsupported = resolve_run_profile(home.path(), "model", RunProfileQuery::default());
        assert_eq!(
            unsupported.unavailable,
            Some(UnavailableReason::UnsupportedTunerVersion)
        );
        assert_eq!(
            unsupported
                .superseded
                .as_ref()
                .map(|entry| entry.tuner_version),
            Some(CURRENT_TUNER_VERSION - 1)
        );
        assert_eq!(
            unsupported.unavailable.map(UnavailableReason::as_str),
            Some("unsupported_tuner_version")
        );
        let warning = unsupported.warning("model").unwrap();
        assert!(warning.contains("templated /v1/chat/completions"));
        assert!(warning.contains("settings:"));
        assert!(warning.contains("measured 2026-08-10T00:00:00Z"));
        assert!(warning.contains("localbench findbest model"));

        let current = entry("q4", 10.0, Profile::Balanced, 24);
        write_store(home.path(), vec![older, current]);
        let selected = resolve_run_profile(home.path(), "model", RunProfileQuery::default());
        assert_eq!(selected.entry.unwrap().score, 10.0);
        assert_eq!(selected.unavailable, None);
        assert_eq!(selected.superseded, None);
        assert_eq!(selected.adoption, None);
    }

    #[test]
    fn future_measurements_are_never_offered_as_adoptable() {
        let home = tempfile::tempdir().unwrap();
        let mut future = entry("q4", 10.0, Profile::Balanced, 24);
        future.tuner_version = CURRENT_TUNER_VERSION + 1;
        write_store(home.path(), vec![future]);

        let resolution = resolve_run_profile(home.path(), "model", RunProfileQuery::default());
        assert_eq!(
            resolution.unavailable,
            Some(UnavailableReason::UnsupportedTunerVersion)
        );
        assert_eq!(resolution.superseded, None);
    }

    #[test]
    fn adoption_preserves_the_document_backs_up_and_applies_in_the_same_resolution() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".local-llm").join("tuner");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("best-model.json");

        let mut selected = entry("q4", 90.0, Profile::Balanced, 24);
        selected.tuner_version = CURRENT_TUNER_VERSION - 1;
        selected.overrides.n_cpu_moe = Some(45);
        let mut untouched = entry("q8", 80.0, Profile::Pure, 48);
        untouched.tuner_version = CURRENT_TUNER_VERSION - 1;
        let mut selected_value = serde_json::to_value(&selected).unwrap();
        selected_value
            .as_object_mut()
            .unwrap()
            .insert("third_party_note".to_string(), "keep me".into());
        let mut untouched_value = serde_json::to_value(&untouched).unwrap();
        untouched_value
            .as_object_mut()
            .unwrap()
            .insert("other_tool_field".to_string(), 42.into());
        let original = serde_json::json!({
            "schema": 1,
            "key": "model",
            "root_unknown": {"preserved": true},
            "entries": [selected_value, untouched_value]
        });
        let original_raw = serde_json::to_string_pretty(&original).unwrap();
        std::fs::write(&path, &original_raw).unwrap();

        let mut resolution = resolve_run_profile(
            home.path(),
            "model",
            RunProfileQuery {
                quant: Some("q4"),
                ..RunProfileQuery::default()
            },
        );
        assert_eq!(
            resolution
                .superseded
                .as_ref()
                .map(|entry| entry.quant.as_str()),
            Some("q4")
        );

        adopt_superseded_profile_at(&mut resolution, "2026-08-28T12:34:56Z").unwrap();

        assert!(resolution.is_tuned());
        assert_eq!(resolution.unavailable, None);
        assert_eq!(resolution.superseded, None);
        assert_eq!(
            resolution.entry.as_ref().unwrap().tuner_version,
            CURRENT_TUNER_VERSION
        );
        assert_eq!(
            resolution.entry.as_ref().unwrap().overrides.n_cpu_moe,
            Some(45)
        );
        assert_eq!(
            resolution.adoption,
            Some(ProfileAdoption {
                from_tuner_version: CURRENT_TUNER_VERSION - 1,
                adopted_at: "2026-08-28T12:34:56Z".to_string(),
            })
        );

        let mut request = LaunchRequest::new("model", "", Mode::Native);
        resolution.apply_to_request(&mut request, false, false, false);
        assert_eq!(request.quant.as_deref(), Some("q4"));
        assert_eq!(request.context_key, "64k");
        assert_eq!(request.params.n_cpu_moe, Some(45));

        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.bak")).unwrap(),
            original_raw
        );
        let written_raw = std::fs::read_to_string(&path).unwrap();
        let original_entries = json_object_field_span(
            &original_raw,
            json_root_span(&original_raw).unwrap(),
            "entries",
        )
        .unwrap();
        let original_selected = json_array_item_span(&original_raw, original_entries, 0).unwrap();
        let written_entries = json_object_field_span(
            &written_raw,
            json_root_span(&written_raw).unwrap(),
            "entries",
        )
        .unwrap();
        let written_selected = json_array_item_span(&written_raw, written_entries, 0).unwrap();
        assert_eq!(
            &written_raw[..written_selected.start],
            &original_raw[..original_selected.start],
            "every byte before the selected entry stays untouched"
        );
        assert_eq!(
            &written_raw[written_selected.end..],
            &original_raw[original_selected.end..],
            "every other entry and root field stays byte-identical"
        );
        assert!(written_raw[written_selected].contains("\"third_party_note\": \"keep me\""));

        let written: serde_json::Value = serde_json::from_str(&written_raw).unwrap();
        let mut expected = original;
        let expected_entry = expected["entries"][0].as_object_mut().unwrap();
        expected_entry.insert("tuner_version".to_string(), CURRENT_TUNER_VERSION.into());
        expected_entry.insert(
            ADOPTED_FROM_FIELD.to_string(),
            (CURRENT_TUNER_VERSION - 1).into(),
        );
        expected_entry.insert(ADOPTED_AT_FIELD.to_string(), "2026-08-28T12:34:56Z".into());
        assert_eq!(
            written, expected,
            "only the selected entry gains adoption fields"
        );

        let reloaded = resolve_run_profile(
            home.path(),
            "model",
            RunProfileQuery {
                quant: Some("q4"),
                ..RunProfileQuery::default()
            },
        );
        assert_eq!(reloaded.entry.as_ref().unwrap().quant, "q4");
        assert_eq!(reloaded.adoption, resolution.adoption);
    }

    #[test]
    fn adoption_timestamps_are_utc_rfc3339_seconds() {
        assert_eq!(format_utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc_timestamp(946_684_800), "2000-01-01T00:00:00Z");
    }

    #[test]
    fn a_later_adoption_replaces_provenance_fields_without_duplicate_keys() {
        let raw = r#"{"schema":1,"key":"m","entries":[{"quant":"q4","contextKey":"64k","mode":"native","vramGB":24,"prompt_length":"short","profile":"balanced","score":1,"scoreUnit":"tok/s","args":[],"overrides":{},"measured_at":"then","tuner_version":4,"localbox_adopted_from_tuner_version":3,"localbox_adopted_at":"before"}],"tail":"unchanged"}"#;
        let patched = patched_adoption_json(raw, 0, 4, "2026-08-28T12:34:56Z").unwrap();
        assert_eq!(patched.matches(ADOPTED_FROM_FIELD).count(), 1);
        assert_eq!(patched.matches(ADOPTED_AT_FIELD).count(), 1);
        assert!(patched.contains("\"localbox_adopted_from_tuner_version\":4"));
        assert!(patched.contains("\"localbox_adopted_at\":\"2026-08-28T12:34:56Z\""));
        assert!(patched.ends_with("}],\"tail\":\"unchanged\"}"));
    }
}
