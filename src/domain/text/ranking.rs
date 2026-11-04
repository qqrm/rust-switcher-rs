//! Scores already-generated keyboard-layout candidates using local character models.
//!
//! Language identification confidence is not a spelling score: Lingua can classify a
//! nonsense string from its alphabet alone. This module reads the same model crates'
//! public FST resources and measures character-sequence evidence directly instead.
//!
//! Model format: Lingua 1.8 `src/model.rs` and `src/file.rs`, model crates 1.3.0.
//! `ngrams.fst` maps UTF-8 n-grams (lengths 1..=5) to `f64::to_bits(ln(p))`;
//! higher-order probabilities are conditional on the preceding characters.
//! https://github.com/pemistahl/lingua-rs/blob/v1.8.0/src/model.rs

#[path = "model_cache.rs"]
mod model_cache;
#[path = "model_catalog.rs"]
mod model_catalog;

use std::sync::Arc;

use fst::Streamer;
use model_cache::ProbabilityModel;
use model_catalog::{ModelSpec, language_for_tag};

const MAX_SCORING_CHARS: usize = 512;
const MAX_NGRAM_LENGTH: usize = 5;
const UNKNOWN_LOG_PROBABILITY: f64 = -12.0;
const BACKOFF_PENALTY: f64 = 0.8;
const MIN_ALPHABET_COVERAGE: f64 = 0.95;
// Conservative decision thresholds exercised by real model fixtures. These are
// evidence scores, not calibrated language probabilities or error guarantees.
const MANUAL_MIN_SCORE: f64 = -4.0;
const AUTOMATIC_MIN_SCORE: f64 = -2.8;
const PLAUSIBLE_ORIGINAL_SCORE: f64 = -3.5;
const MANUAL_MIN_MARGIN: f64 = 0.3;
const AUTOMATIC_MIN_MARGIN: f64 = 1.0;

#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    pub text: String,
    pub language: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RankingMode {
    Manual,
    Automatic,
}

#[derive(Clone, Copy, Debug)]
struct Evidence {
    score: f64,
    alphabet_coverage: f64,
}

/// Warms only models corresponding to the installed Windows language tags.
///
/// EN/RU are bundled. Other languages enqueue a deduplicated background cache
/// load or verified download. This function never waits for disk or network I/O.
/// Models are immutable and shared with subsequent ranking calls.
pub(crate) fn warm_languages(languages: &[String]) {
    for language in languages.iter().filter_map(|tag| language_for_tag(tag)) {
        if let Some(model) = model_cache::model(language) {
            // Touch the FST's root and some entries on the warming thread.
            let mut stream = model.stream();
            for _ in 0..512 {
                if stream.next().is_none() {
                    break;
                }
            }
        }
    }
}

/// Returns the index of a convincing candidate, or `None` when the available
/// evidence is insufficient. Scores are never interpreted as probabilities.
///
/// Explicit commands may omit the unchanged candidate; their caller may fall
/// back to a deterministic layout cycle. Smart commands should include it.
/// Automatic correction must include it and must honor `None` as "do nothing".
pub(crate) fn rank_candidates(
    source: &str,
    candidates: &[Candidate],
    mode: RankingMode,
) -> Option<usize> {
    let min_letters = match mode {
        RankingMode::Manual => 3,
        RankingMode::Automatic => 4,
    };
    if candidates.is_empty() || letter_count(source) < min_letters {
        return None;
    }

    let original_index = candidates
        .iter()
        .position(|candidate| candidate.text == source);
    if mode == RankingMode::Automatic && (original_index.is_none() || looks_like_identifier(source))
    {
        return None;
    }

    let models: Vec<_> = candidates
        .iter()
        .map(|candidate| {
            let language = language_for_tag(candidate.language.as_deref()?)?;
            Some((language, model_cache::model(language)?))
        })
        .collect();

    // A missing model cannot be treated as evidence against that layout. This
    // also applies to unchanged candidates: their language may make the source
    // valid even if another installed language gives it a low score. Supplying
    // an unchanged candidate makes this conservative rule apply to smart
    // commands too; explicit conversion can still use its deterministic cycle.
    if original_index.is_some() && models.iter().any(Option::is_none) {
        return None;
    }

    let original_evidence = score_original(source, &models);
    if mode == RankingMode::Automatic
        && original_evidence
            .is_none_or(|evidence| evidence.alphabet_coverage < MIN_ALPHABET_COVERAGE)
    {
        return None;
    }
    if mode == RankingMode::Automatic
        && original_evidence.is_some_and(|evidence| {
            evidence.alphabet_coverage >= MIN_ALPHABET_COVERAGE
                && evidence.score >= PLAUSIBLE_ORIGINAL_SCORE
        })
    {
        return original_index;
    }

    let mut scores = Vec::with_capacity(candidates.len());
    for (index, (candidate, model)) in candidates.iter().zip(&models).enumerate() {
        if letter_count(&candidate.text) < min_letters {
            continue;
        }
        let evidence = if candidate.text == source {
            original_evidence
        } else {
            model
                .as_ref()
                .and_then(|(language, model)| score_text(&candidate.text, language, model))
        };
        if let Some(evidence) = evidence
            && evidence.alphabet_coverage >= MIN_ALPHABET_COVERAGE
        {
            scores.push((index, evidence.score));
        }
    }
    scores.sort_by(|left, right| right.1.total_cmp(&left.1).then(left.0.cmp(&right.0)));
    let &(best_index, best_score) = scores.first()?;

    if candidates[best_index].text == source {
        return original_index;
    }

    let (min_score, min_margin) = match mode {
        RankingMode::Manual => (MANUAL_MIN_SCORE, MANUAL_MIN_MARGIN),
        RankingMode::Automatic => (AUTOMATIC_MIN_SCORE, AUTOMATIC_MIN_MARGIN),
    };
    if best_score < min_score {
        return None;
    }

    // Identical output from two layout variants is one textual candidate, not
    // an artificial tie. Their existing order supplies a deterministic target.
    let competing_score = scores
        .iter()
        .skip(1)
        .find(|(index, _)| candidates[*index].text != candidates[best_index].text)
        .map(|(_, score)| *score);
    if competing_score.is_some_and(|score| best_score - score < min_margin) {
        return None;
    }

    // Also compare against a poorly-scoring original that did not qualify for
    // the ranked list. Unknown evidence cannot authorize an automatic change.
    if mode == RankingMode::Automatic {
        let original = original_evidence?;
        if best_score - original.score < AUTOMATIC_MIN_MARGIN {
            return None;
        }
    }
    Some(best_index)
}

fn letter_count(text: &str) -> usize {
    text.chars()
        .take(MAX_SCORING_CHARS)
        .filter(|character| character.is_alphabetic())
        .count()
}

fn looks_like_identifier(text: &str) -> bool {
    if text.chars().any(|character| {
        matches!(
            character,
            '@' | '/' | '\\' | '_' | '`' | '=' | '<' | '>' | '$' | '|'
        )
    }) {
        return true;
    }
    if (text.contains('[') && text.contains(']')) || (text.contains('{') && text.contains('}')) {
        return true;
    }
    for token in text.split_whitespace() {
        if token.chars().any(|character| character.is_ascii_digit())
            || token.contains("::")
            || token.chars().collect::<Vec<_>>().windows(3).any(|chars| {
                chars[0].is_alphanumeric() && chars[1] == '.' && chars[2].is_alphanumeric()
            })
        {
            return true;
        }
        let mut lower_seen = false;
        for character in token.chars() {
            if lower_seen && character.is_uppercase() {
                return true;
            }
            lower_seen |= character.is_lowercase();
        }
    }
    false
}

fn score_original(
    source: &str,
    models: &[Option<(&'static ModelSpec, Arc<ProbabilityModel>)>],
) -> Option<Evidence> {
    // Correct mixed-language input gets the benefit of each installed model
    // for each word. This intentionally favors preserving valid user text.
    let mut weighted_score = 0.0;
    let mut weighted_coverage = 0.0;
    let mut letters = 0usize;
    let bounded: String = source.chars().take(MAX_SCORING_CHARS).collect();
    for token in bounded.split(|character: char| !character.is_alphabetic()) {
        if token.is_empty() {
            continue;
        }
        let best = models
            .iter()
            .filter_map(Option::as_ref)
            .filter_map(|(language, model)| score_text(token, language, model))
            .max_by(|left, right| left.score.total_cmp(&right.score))?;
        let count = token.chars().count();
        weighted_score += best.score * count as f64;
        weighted_coverage += best.alphabet_coverage * count as f64;
        letters += count;
    }
    if letters == 0 {
        return None;
    }
    Some(Evidence {
        score: weighted_score / letters as f64,
        alphabet_coverage: weighted_coverage / letters as f64,
    })
}

fn score_text(text: &str, language: &ModelSpec, model: &ProbabilityModel) -> Option<Evidence> {
    let lower = normalize_for_scoring(text, language);
    let mut log_probability = 0.0;
    let mut known_letters = 0usize;
    let mut letters = 0usize;
    for token in lower.split(|character: char| !character.is_alphabetic()) {
        let chars: Vec<char> = token.chars().collect();
        for (position, &character) in chars.iter().enumerate() {
            let unigram = character.to_string();
            known_letters += usize::from(model.contains_key(unigram.as_bytes()));
            let order = (position + 1).min(MAX_NGRAM_LENGTH);
            let probability = (1..=order).rev().find_map(|length| {
                let ngram: String = chars[position + 1 - length..=position].iter().collect();
                let value = f64::from_bits(model.get(ngram.as_bytes())?);
                (value.is_finite() && value <= 0.0)
                    .then_some(value - BACKOFF_PENALTY * (order - length) as f64)
            });
            log_probability += probability.unwrap_or(UNKNOWN_LOG_PROBABILITY);
            letters += 1;
        }
    }
    if letters == 0 {
        return None;
    }
    Some(Evidence {
        score: log_probability / letters as f64,
        alphabet_coverage: known_letters as f64 / letters as f64,
    })
}

fn normalize_for_scoring(text: &str, language: &ModelSpec) -> String {
    let turkic = matches!(language.code, "tr" | "az");
    let mut lower = String::new();
    for character in text.chars().take(MAX_SCORING_CHARS) {
        match character {
            'I' if turkic => lower.push('ı'),
            'İ' if turkic => lower.push('i'),
            _ => lower.extend(character.to_lowercase()),
        }
    }
    lower
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(text: &str, language: &str) -> Candidate {
        Candidate {
            text: text.to_owned(),
            language: Some(language.to_owned()),
        }
    }

    #[test]
    fn windows_language_tags_resolve_without_a_small_language_whitelist() {
        for (tag, language) in [
            ("en-US", "en"),
            ("EN_gb", "en"),
            ("de-DE", "de"),
            ("uk-UA", "uk"),
            ("be-BY", "be"),
            ("kk-KZ", "kk"),
            ("sr-Latn-RS", "sr"),
            ("fil-PH", "tl"),
            ("no-NO", "nb"),
            ("eng", "en"),
        ] {
            assert_eq!(
                language_for_tag(tag).map(|model| model.code),
                Some(language),
                "{tag}"
            );
        }
        assert!(language_for_tag("").is_none());
        assert!(language_for_tag("zz-ZZ").is_none());
    }

    #[test]
    fn punctuation_and_short_ambiguous_words_are_left_to_the_layout_policy() {
        for source in [".", "...", "123", "😀", "a", "ok"] {
            let candidates = [candidate(source, "en-US"), candidate("ю", "ru-RU")];
            assert_eq!(
                rank_candidates(source, &candidates, RankingMode::Manual),
                None
            );
            assert_eq!(
                rank_candidates(source, &candidates, RankingMode::Automatic),
                None
            );
        }
    }

    #[test]
    fn explicit_ranking_prefers_a_russian_phrase_over_wrong_layout_text() {
        let candidates = [
            candidate("ghbdtn rfr ltkf", "en-US"),
            candidate("привет как дела", "ru-RU"),
            candidate("ghbdtn rfr ltkf", "de-DE"),
        ];
        assert_eq!(
            rank_candidates("ghbdtn rfr ltkf", &candidates, RankingMode::Manual),
            Some(1)
        );
    }

    #[test]
    fn explicit_ranking_recovers_an_english_phrase() {
        let candidates = [
            candidate("руддщ цщкдв", "ru-RU"),
            candidate("hello world", "en-US"),
            candidate("руддщ цщкдв", "uk-UA"),
        ];
        assert_eq!(
            rank_candidates("руддщ цщкдв", &candidates, RankingMode::Manual),
            Some(1)
        );
    }

    #[test]
    fn ranking_uses_german_sequences_for_qwertz_candidates() {
        let candidates = [
            candidate("die katye spielt draussen", "en-US"),
            candidate("die katze spielt draussen", "de-DE"),
            candidate("вшу лфеян ызшудец вкфгыыут", "ru-RU"),
        ];
        assert_eq!(
            rank_candidates(
                "die katye spielt draussen",
                &candidates,
                RankingMode::Manual
            ),
            Some(1)
        );
    }

    #[test]
    fn ukrainian_models_can_rank_a_phrase_containing_language_specific_letters() {
        let candidates = [
            candidate("привіт як справи", "uk-UA"),
            candidate("ghbdsn zr cghfdb", "en-US"),
            candidate("привит як справи", "ru-RU"),
        ];
        assert_eq!(
            rank_candidates("ghbdsn zr cghfdb", &candidates, RankingMode::Manual),
            Some(0)
        );
    }

    #[test]
    fn automatic_ranking_preserves_valid_text_and_mixed_language_input() {
        for source in [
            "hello world",
            "привет как дела",
            "hello привет",
            "Rust compiler",
        ] {
            let candidates = [
                candidate(source, "en-US"),
                candidate(source, "ru-RU"),
                candidate("привет мир", "ru-RU"),
            ];
            assert_eq!(
                rank_candidates(source, &candidates, RankingMode::Automatic),
                Some(0),
                "{source}"
            );
        }
    }

    #[test]
    fn automatic_ranking_requires_original_evidence() {
        let candidates = [candidate("привет", "ru-RU")];
        assert_eq!(
            rank_candidates("ghbdtn", &candidates, RankingMode::Automatic),
            None
        );
    }

    #[test]
    fn automatic_ranking_recovers_unambiguous_wrong_layout_text() {
        for (source, target) in [
            ("ghbdtn", "привет"),
            ("ghbdtn rfr ltkf", "привет как дела"),
            ("[jhjij", "хорошо"),
        ] {
            let candidates = [candidate(source, "en-US"), candidate(target, "ru-RU")];
            assert_eq!(
                rank_candidates(source, &candidates, RankingMode::Automatic),
                Some(1),
                "{source}"
            );
        }
    }

    #[test]
    fn automatic_ranking_distinguishes_real_russian_ukrainian_physical_candidates() {
        // The hosted native planner tests independently verify these physical
        // outputs against the installed Windows layouts. In particular, US s
        // is Russian ы and Ukrainian і: ghbdsn does not produce Russian привит.
        for (source, russian, ukrainian, french, manual, automatic) in [
            (
                "ghbdsn",
                "привыт",
                "привіт",
                "ghbdsn",
                "привіт",
                Some("привіт"),
            ),
            ("dshsnm", "вырыть", "віріть", "dshsn,", "вырыть", None),
        ] {
            let mut candidates = vec![
                candidate(russian, "ru-RU"),
                candidate(ukrainian, "uk-UA"),
                candidate(french, "fr-FR"),
            ];
            for language in ["en-US", "ru-RU", "uk-UA", "de-DE", "fr-FR", "pl-PL"] {
                candidates.push(candidate(source, language));
            }
            let chosen = |mode| {
                rank_candidates(source, &candidates, mode)
                    .map(|index| candidates[index].text.as_str())
            };
            assert_eq!(chosen(RankingMode::Manual), Some(manual), "{source}");
            assert_eq!(chosen(RankingMode::Automatic), automatic, "{source}");
        }
    }

    #[test]
    fn automatic_ranking_abstains_when_original_script_has_no_model() {
        let candidates = [candidate("你好世界", "zz-ZZ"), candidate("привет", "ru-RU")];
        assert_eq!(
            rank_candidates("你好世界", &candidates, RankingMode::Automatic),
            None
        );
    }

    #[test]
    fn identical_layout_outputs_do_not_create_false_ambiguity() {
        let candidates = [
            candidate("ghbdtn", "en-US"),
            candidate("привет", "ru-RU"),
            candidate("привет", "ru-RU"),
        ];
        assert_eq!(
            rank_candidates("ghbdtn", &candidates, RankingMode::Automatic),
            Some(1)
        );
    }

    #[test]
    fn automatic_ranking_does_not_rewrite_common_code_or_address_shapes() {
        for source in [
            "result_value",
            "someValue",
            "qqrm@example.org",
            "example.org",
            "https://example.org",
            "Vec<u32>",
            "123abc",
        ] {
            let candidates = [candidate(source, "en-US"), candidate("привет", "ru-RU")];
            assert_eq!(
                rank_candidates(source, &candidates, RankingMode::Automatic),
                None,
                "{source}"
            );
        }
    }

    #[test]
    fn unavailable_target_models_cannot_authorize_automatic_replacement() {
        let candidates = [
            candidate("ghbdtn", "en-US"),
            candidate("привет", "ru-RU"),
            candidate("some unknown target", "zz-ZZ"),
        ];
        assert_eq!(
            rank_candidates("ghbdtn", &candidates, RankingMode::Automatic),
            None
        );
    }

    #[test]
    fn unavailable_original_models_cannot_authorize_smart_or_automatic_replacement() {
        let candidates = [
            candidate("ghbdtn", "en-US"),
            candidate("ghbdtn", "zz-ZZ"),
            candidate("привет", "ru-RU"),
        ];
        for mode in [RankingMode::Manual, RankingMode::Automatic] {
            assert_eq!(rank_candidates("ghbdtn", &candidates, mode), None);
        }
    }

    #[test]
    fn script_only_gibberish_does_not_receive_a_perfect_score() {
        let language = language_for_tag("ru-RU").expect("Russian manifest missing");
        let Some(model) = model_cache::model(language) else {
            panic!("Russian model missing");
        };
        let good = score_text("привет как дела", language, &model);
        let bad = score_text("фывапролдж щзхъ", language, &model);
        assert!(matches!((good, bad), (Some(good), Some(bad)) if good.score > bad.score + 1.0));
    }

    #[test]
    fn turkic_case_folding_preserves_dotted_and_dotless_i() {
        assert_eq!(
            normalize_for_scoring(
                "IŞIK İÇİN",
                language_for_tag("tr-TR").expect("Turkish manifest missing")
            ),
            "ışık için"
        );
    }

    #[test]
    fn cold_and_warmed_ranking_agree() {
        let candidates = [candidate("ghbdtn", "en-US"), candidate("привет", "ru-RU")];
        let cold = rank_candidates("ghbdtn", &candidates, RankingMode::Manual);
        warm_languages(&["en-US".to_owned(), "ru-RU".to_owned()]);
        assert_eq!(
            rank_candidates("ghbdtn", &candidates, RankingMode::Manual),
            cold
        );
        assert_eq!(cold, Some(1));
    }
}
