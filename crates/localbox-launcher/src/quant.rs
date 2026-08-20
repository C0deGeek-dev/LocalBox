//! Quant discovery from a Hugging Face repo's GGUF file list.
//!
//! A GGUF repo publishes one file per quantisation (`…-Q4_K_M.gguf`,
//! `….i1-IQ4_XS.gguf`), sometimes split into ordered shards
//! (`…-00001-of-00003.gguf`). This module reads those filenames — the naming
//! conventions mradermacher, bartowski, and TheBloke all use — into a stable set
//! of selectable [`QuantCandidate`]s, and resolves a caller's `--quant` request
//! (or a sensible default) to one. It is pure: it interprets the listing from
//! [`hf_meta`](crate::hf_meta), it does not fetch or write anything.

use std::collections::BTreeMap;

use crate::hf_meta::HfGgufFile;

/// One selectable quant variant: a stable key, the file(s) that make it up (a
/// single file, or shards in order), and their combined size when the Hub
/// reported every part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuantCandidate {
    pub key: String,
    pub files: Vec<String>,
    pub total_size: Option<u64>,
}

/// Why a quant could not be selected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QuantSelectError {
    /// No GGUF quant variants were discovered in the listing.
    #[error("no GGUF quant variants were found")]
    NoneAvailable,
    /// A requested quant key matched none of the candidates.
    #[error("quant '{requested}' not found; available: {available}")]
    Unknown {
        requested: String,
        available: String,
    },
}

/// The compact, comparison-friendly form of a quant token or key: lowercase with
/// the `_`/`-` separators removed (`Q4_K_M` → `q4km`, `i1-IQ4_XS` → `i1iq4xs`).
fn compact(token: &str) -> String {
    token
        .to_ascii_lowercase()
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .collect()
}

/// Whether a filename segment names a GGUF quantisation (`Q4_K_M`, `IQ4_XS`,
/// `Q8_0`, `F16`, …). The check is deliberately narrow so a model name that
/// merely starts with `q` (e.g. `qwen`) is not mistaken for a quant.
fn is_quant_token(segment: &str) -> bool {
    let seg = segment.to_ascii_lowercase();
    if matches!(seg.as_str(), "f16" | "f32" | "bf16") {
        return true;
    }
    if let Some(rest) = seg.strip_prefix("iq") {
        return rest.starts_with(|c: char| c.is_ascii_digit());
    }
    if let Some(rest) = seg.strip_prefix('q') {
        return rest.starts_with(|c: char| c.is_ascii_digit());
    }
    false
}

/// Split a shard suffix (`…-00001-of-00003`) off a filename stem, returning the
/// base stem and the `(index, total)` when present.
fn split_shard(stem: &str) -> (&str, Option<(u32, u32)>) {
    let Some(of_pos) = stem.rfind("-of-") else {
        return (stem, None);
    };
    let before = &stem[..of_pos];
    let total_str = &stem[of_pos + "-of-".len()..];
    let Some(dash) = before.rfind('-') else {
        return (stem, None);
    };
    let index_str = &before[dash + 1..];
    let is_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if is_digits(index_str) && is_digits(total_str) {
        let index = index_str.parse().unwrap_or(0);
        let total = total_str.parse().unwrap_or(0);
        (&before[..dash], Some((index, total)))
    } else {
        (stem, None)
    }
}

/// The quant key a GGUF filename denotes, or `None` when the name carries no
/// recognisable quant token. An imatrix marker (`i1-`/`imat`) is preserved so an
/// imatrix build and a static build of the same quant get distinct keys.
#[must_use]
pub fn quant_key_from_filename(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".gguf")?;
    let (base, _shard) = split_shard(stem);
    let segments: Vec<&str> = base.split(['.', '-']).filter(|s| !s.is_empty()).collect();
    let idx = segments.iter().rposition(|s| is_quant_token(s))?;
    let key = compact(segments[idx]);
    let imatrix = idx > 0 && matches!(segments[idx - 1], "i1" | "imat" | "imatrix");
    Some(if imatrix { format!("i1-{key}") } else { key })
}

/// The shard index a GGUF filename carries (`1` when it is not sharded), used to
/// order the files of a multi-part candidate.
fn shard_index(name: &str) -> u32 {
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".gguf").unwrap_or(&lower);
    split_shard(stem).1.map_or(1, |(index, _)| index)
}

/// Group a repo's GGUF files into selectable quant candidates: one per quant
/// key, shards ordered, sizes summed when every part's size is known. Sorted by
/// key for stable display.
#[must_use]
pub fn quant_candidates(files: &[HfGgufFile]) -> Vec<QuantCandidate> {
    let mut groups: BTreeMap<String, Vec<&HfGgufFile>> = BTreeMap::new();
    for file in files {
        if let Some(key) = quant_key_from_filename(&file.rfilename) {
            groups.entry(key).or_default().push(file);
        }
    }
    groups
        .into_iter()
        .map(|(key, mut parts)| {
            parts.sort_by_key(|f| shard_index(&f.rfilename));
            let total_size = parts
                .iter()
                .try_fold(0u64, |acc, f| f.size.map(|s| acc + s));
            let files = parts.iter().map(|f| f.rfilename.clone()).collect();
            QuantCandidate {
                key,
                files,
                total_size,
            }
        })
        .collect()
}

/// A compact quant key with any imatrix marker dropped, so `q4km` and its
/// imatrix twin `i1-q4km` compare equal (`i1q4km` → `q4km`).
fn without_imatrix(compact_key: &str) -> &str {
    compact_key.strip_prefix("i1").unwrap_or(compact_key)
}

/// The index of the default candidate: `Q4_K_M` (static or imatrix) when
/// present, otherwise the median-by-size when every size is known, otherwise the
/// middle of the key-sorted list. `candidates` must be non-empty. An
/// imatrix-only repo (every quant `i1-…`, as mradermacher publishes) has no
/// plain `q4km`, so the imatrix twin is accepted as the default too.
fn default_index(candidates: &[QuantCandidate]) -> usize {
    if let Some(i) = candidates
        .iter()
        .position(|c| without_imatrix(&compact(&c.key)) == "q4km")
    {
        return i;
    }
    if candidates.iter().all(|c| c.total_size.is_some()) {
        let mut order: Vec<usize> = (0..candidates.len()).collect();
        order.sort_by_key(|&i| candidates[i].total_size);
        order[order.len() / 2]
    } else {
        candidates.len() / 2
    }
}

/// Resolve a caller's `--quant` request (or, when `None`, a default) to one
/// candidate. A request matches a key case-insensitively, by compact form
/// (`Q4_K_M` matches `q4km`), or — when the match is unambiguous — ignoring an
/// imatrix marker, so `--quant q4km` selects `i1-q4km` in an imatrix-only repo.
///
/// # Errors
/// [`QuantSelectError::NoneAvailable`] when the list is empty;
/// [`QuantSelectError::Unknown`] when a requested key matches nothing.
pub fn select_quant(
    mut candidates: Vec<QuantCandidate>,
    requested: Option<&str>,
) -> Result<QuantCandidate, QuantSelectError> {
    if candidates.is_empty() {
        return Err(QuantSelectError::NoneAvailable);
    }
    match requested {
        Some(req) => {
            let pos = match_quant(&candidates, req).ok_or_else(|| QuantSelectError::Unknown {
                requested: req.to_string(),
                available: candidates
                    .iter()
                    .map(|c| c.key.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            })?;
            Ok(candidates.swap_remove(pos))
        }
        None => {
            let index = default_index(&candidates);
            Ok(candidates.swap_remove(index))
        }
    }
}

/// The candidate index a `--quant` request selects: an exact key / compact match
/// first, then — only when it picks out exactly one candidate — an
/// imatrix-insensitive match.
fn match_quant(candidates: &[QuantCandidate], req: &str) -> Option<usize> {
    let want = compact(req);
    if let Some(i) = candidates
        .iter()
        .position(|c| c.key.eq_ignore_ascii_case(req) || compact(&c.key) == want)
    {
        return Some(i);
    }
    let want_bare = without_imatrix(&want);
    let fuzzy: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| without_imatrix(&compact(&c.key)) == want_bare)
        .map(|(i, _)| i)
        .collect();
    (fuzzy.len() == 1).then(|| fuzzy[0])
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn file(name: &str, size: Option<u64>) -> HfGgufFile {
        HfGgufFile {
            rfilename: name.to_string(),
            size,
        }
    }

    #[test]
    fn quant_keys_cover_the_real_naming_conventions() {
        // bartowski static, mradermacher static + imatrix, other tokens.
        assert_eq!(
            quant_key_from_filename("Llama-3.2-1B-Instruct-Q4_K_M.gguf").as_deref(),
            Some("q4km")
        );
        assert_eq!(
            quant_key_from_filename("Some-Model.Q4_K_M.gguf").as_deref(),
            Some("q4km")
        );
        assert_eq!(
            quant_key_from_filename("Some-Model.i1-Q4_K_M.gguf").as_deref(),
            Some("i1-q4km")
        );
        assert_eq!(
            quant_key_from_filename("Model-IQ4_XS.gguf").as_deref(),
            Some("iq4xs")
        );
        assert_eq!(
            quant_key_from_filename("Model-Q8_0.gguf").as_deref(),
            Some("q80")
        );
        assert_eq!(
            quant_key_from_filename("Model-f16.gguf").as_deref(),
            Some("f16")
        );
        // A name with no quant token, and a non-gguf file.
        assert_eq!(quant_key_from_filename("Qwen3-27B.gguf"), None);
        assert_eq!(quant_key_from_filename("README.md"), None);
    }

    #[test]
    fn multipart_shards_group_into_one_candidate_with_summed_size() {
        let files = vec![
            file("Big-Model-Q6_K-00002-of-00003.gguf", Some(20)),
            file("Big-Model-Q6_K-00001-of-00003.gguf", Some(10)),
            file("Big-Model-Q6_K-00003-of-00003.gguf", Some(30)),
            file("Big-Model-Q4_K_M.gguf", Some(5)),
        ];
        let candidates = quant_candidates(&files);
        // Sorted by key: q4km, q6k.
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].key, "q4km");
        assert_eq!(candidates[0].files, vec!["Big-Model-Q4_K_M.gguf"]);
        assert_eq!(candidates[0].total_size, Some(5));

        let q6k = &candidates[1];
        assert_eq!(q6k.key, "q6k");
        assert_eq!(
            q6k.files,
            vec![
                "Big-Model-Q6_K-00001-of-00003.gguf",
                "Big-Model-Q6_K-00002-of-00003.gguf",
                "Big-Model-Q6_K-00003-of-00003.gguf",
            ],
            "shards are ordered by index"
        );
        assert_eq!(q6k.total_size, Some(60));
    }

    #[test]
    fn a_missing_shard_size_makes_the_total_unknown() {
        let files = vec![
            file("M-Q6_K-00001-of-00002.gguf", Some(10)),
            file("M-Q6_K-00002-of-00002.gguf", None),
        ];
        let candidates = quant_candidates(&files);
        assert_eq!(candidates[0].total_size, None);
        assert_eq!(candidates[0].files.len(), 2);
    }

    #[test]
    fn a_requested_quant_matches_by_key_or_compact_form() {
        let candidates = quant_candidates(&[
            file("M-Q4_K_M.gguf", Some(4)),
            file("M-IQ4_XS.gguf", Some(3)),
        ]);
        assert_eq!(
            select_quant(candidates.clone(), Some("iq4xs")).unwrap().key,
            "iq4xs"
        );
        // The user can paste the on-disk spelling too.
        assert_eq!(
            select_quant(candidates.clone(), Some("Q4_K_M"))
                .unwrap()
                .key,
            "q4km"
        );
        let err = select_quant(candidates, Some("q2k")).unwrap_err();
        assert!(matches!(err, QuantSelectError::Unknown { .. }));
    }

    #[test]
    fn the_default_prefers_q4km_then_median_size() {
        // q4km present → picked regardless of size.
        let with_q4km = quant_candidates(&[
            file("M-Q2_K.gguf", Some(2)),
            file("M-Q4_K_M.gguf", Some(4)),
            file("M-Q8_0.gguf", Some(8)),
        ]);
        assert_eq!(select_quant(with_q4km, None).unwrap().key, "q4km");

        // No q4km → median by size (of iq2xs=1, q5km=5, q8_0=8 → q5km).
        let without_q4km = quant_candidates(&[
            file("M-IQ2_XS.gguf", Some(1)),
            file("M-Q5_K_M.gguf", Some(5)),
            file("M-Q8_0.gguf", Some(8)),
        ]);
        assert_eq!(select_quant(without_q4km, None).unwrap().key, "q5km");
    }

    #[test]
    fn selecting_from_an_empty_list_is_an_error() {
        assert_eq!(
            select_quant(Vec::new(), None).unwrap_err(),
            QuantSelectError::NoneAvailable
        );
    }

    #[test]
    fn an_imatrix_only_repo_resolves_like_a_real_mradermacher_listing() {
        // A slice of the real Qwen3.8-27B i1-GGUF repo: every quant is imatrix,
        // and the repo also ships a non-model `imatrix.gguf` data file.
        let base = "Qwen3.8-27B-Uncensored-Heretic-Abliterated";
        let files = vec![
            file(&format!("{base}.i1-IQ4_XS.gguf"), Some(15_082_507_808)),
            file(&format!("{base}.i1-Q4_K_M.gguf"), Some(16_547_401_248)),
            file(&format!("{base}.i1-Q6_K.gguf"), Some(22_082_530_848)),
            file(&format!("{base}.i1-IQ1_S.gguf"), Some(7_149_825_568)),
            file(&format!("{base}.imatrix.gguf"), Some(13_642_624)),
        ];
        let candidates = quant_candidates(&files);
        let keys: Vec<&str> = candidates.iter().map(|c| c.key.as_str()).collect();
        // The imatrix data file carries no quant token → excluded; the rest keep
        // their imatrix marker.
        assert_eq!(keys, vec!["i1-iq1s", "i1-iq4xs", "i1-q4km", "i1-q6k"]);

        // The user can ask for the quant as it appears on Hugging Face, without
        // knowing the imatrix prefix.
        for req in ["q4km", "Q4_K_M", "i1-q4km", "i1-Q4_K_M"] {
            assert_eq!(
                select_quant(candidates.clone(), Some(req)).unwrap().key,
                "i1-q4km",
                "request {req:?}"
            );
        }
        // Default prefers the Q4_K_M imatrix twin even though no plain q4km exists.
        assert_eq!(select_quant(candidates, None).unwrap().key, "i1-q4km");
    }
}
