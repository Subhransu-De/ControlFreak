use unicode_normalization::UnicodeNormalization;

use crate::{
    OcrLine, OcrMatchDetails, OcrRegionRequest, PlatformError, TextMatch, TextMatchMode,
    TextMatchTier,
};

const CANDIDATE_LIMIT: usize = 20;
const TEXT_LIMIT: usize = 256;

fn folded(text: &str, case_sensitive: bool) -> String {
    if case_sensitive {
        text.trim().to_owned()
    } else {
        text.trim().to_lowercase()
    }
}

fn normalized(text: &str, case_sensitive: bool) -> String {
    let text: String = text
        .nfkc()
        .map(|ch| match ch {
            '\u{2018}' | '\u{2019}' => '\'',
            '\u{201c}' | '\u{201d}' => '"',
            '\u{2010}'..='\u{2014}' | '\u{2212}' => '-',
            _ => ch,
        })
        .collect();
    folded(
        &text.split_whitespace().collect::<Vec<_>>().join(" "),
        case_sensitive,
    )
}

fn in_region(line: &OcrLine, region: &OcrRegionRequest) -> bool {
    line.width > 0
        && line.height > 0
        && line.x >= region.x
        && line.y >= region.y
        && u64::from(line.x) + u64::from(line.width)
            <= u64::from(region.x) + u64::from(region.width)
        && u64::from(line.y) + u64::from(line.height)
            <= u64::from(region.y) + u64::from(region.height)
}

fn candidate(line: &OcrLine) -> TextMatch {
    TextMatch {
        text: line.text.clone(),
        x: line.x,
        y: line.y,
        width: line.width,
        height: line.height,
    }
}

fn bounded(candidates: &[TextMatch], tier: Option<TextMatchTier>, limit: usize) -> OcrMatchDetails {
    let mut text_truncated = false;
    let limited = candidates
        .iter()
        .take(limit)
        .cloned()
        .map(|mut item| {
            if let Some((offset, _)) = item.text.char_indices().nth(TEXT_LIMIT) {
                item.text.truncate(offset);
                text_truncated = true;
            }
            item
        })
        .collect();
    OcrMatchDetails {
        candidates: limited,
        candidate_count: candidates.len(),
        candidates_truncated: candidates.len() > limit,
        text_truncated,
        match_tier: tier,
        recovery: if tier.is_none() {
            "Refine the query using recognized text, refresh the region, or change match_mode for discovery."
        } else if candidates.len() > 1 {
            "Narrow the region or refine the query. Use exact mode if substring matching is too broad."
        } else {
            "Discovery does not authorize input or verify control state. Confirm the target before acting."
        }.to_owned(),
    }
}

fn validate(query: &str, mode: TextMatchMode, confusions: bool) -> Result<(), PlatformError> {
    if query.trim().is_empty() {
        return Err(PlatformError::InvalidArgument {
            argument: "query".into(),
            reason: "must not be empty".into(),
        });
    }
    if confusions && mode != TextMatchMode::Tolerant {
        return Err(PlatformError::InvalidArgument {
            argument: "ocr_confusions".into(),
            reason: "requires tolerant discovery mode".into(),
        });
    }
    Ok(())
}

// Only one substitution from a fixed confusion group is allowed; no edit-distance ranking.
fn one_confusion(text: &str, query: &str) -> bool {
    let group = |ch| match ch {
        '0' | 'O' | 'o' => 1,
        '1' | 'I' | 'i' | 'l' | '|' => 2,
        _ => 0,
    };
    let a: Vec<_> = text.chars().take(65).collect();
    let b: Vec<_> = query.chars().take(65).collect();
    if !(4..=64).contains(&a.len()) || a.len() != b.len() {
        return false;
    }
    let mut differences = 0;
    for (a, b) in a.into_iter().zip(b) {
        if a != b {
            if group(a) == 0 || group(a) != group(b) {
                return false;
            }
            differences += 1;
        }
    }
    differences == 1
}

fn matching(
    lines: &[OcrLine],
    region: &OcrRegionRequest,
    query: &str,
    case_sensitive: bool,
    mode: TextMatchMode,
    confusions: bool,
) -> (Vec<TextMatch>, Option<TextMatchTier>) {
    let mut lines: Vec<_> = lines
        .iter()
        .filter(|line| in_region(line, region))
        .collect();
    lines.sort_by(|a, b| {
        (a.y, a.x, a.height, a.width, &a.text).cmp(&(b.y, b.x, b.height, b.width, &b.text))
    });
    let query = folded(query, case_sensitive);
    let exact: Vec<_> = lines
        .iter()
        .filter(|line| folded(&line.text, case_sensitive) == query)
        .map(|line| candidate(line))
        .collect();
    if mode != TextMatchMode::Substring && !exact.is_empty() {
        return (exact, Some(TextMatchTier::Exact));
    }
    if mode == TextMatchMode::Substring {
        let matches = lines
            .iter()
            .filter(|line| folded(&line.text, case_sensitive).contains(&query))
            .map(|line| candidate(line))
            .collect::<Vec<_>>();
        let tier = (!matches.is_empty()).then_some(TextMatchTier::Substring);
        return (matches, tier);
    }
    if mode == TextMatchMode::Tolerant {
        let query = normalized(&query, case_sensitive);
        let matches: Vec<_> = lines
            .iter()
            .filter(|line| normalized(&line.text, case_sensitive) == query)
            .map(|line| candidate(line))
            .collect();
        if !matches.is_empty() {
            return (matches, Some(TextMatchTier::Normalized));
        }
        // Join only consecutive, vertically adjacent, left-aligned lines. Never join columns.
        let joined: Vec<_> = lines
            .windows(2)
            .filter_map(|pair| {
                let (a, b) = (pair[0], pair[1]);
                let bottom = u64::from(a.y) + u64::from(a.height);
                if u64::from(b.y) < bottom
                    || u64::from(b.y) - bottom > u64::from(a.height.min(b.height))
                    || a.x.abs_diff(b.x) > a.height.min(b.height) / 2
                {
                    return None;
                }
                let text = format!("{} {}", a.text.trim(), b.text.trim());
                if text.chars().count() > TEXT_LIMIT || normalized(&text, case_sensitive) != query {
                    return None;
                }
                let x = a.x.min(b.x);
                let right =
                    (u64::from(a.x) + u64::from(a.width)).max(u64::from(b.x) + u64::from(b.width));
                Some(TextMatch {
                    text,
                    x,
                    y: a.y,
                    width: u32::try_from(right - u64::from(x)).ok()?,
                    height: b.y.checked_sub(a.y)?.checked_add(b.height)?,
                })
            })
            .collect();
        if !joined.is_empty() {
            return (joined, Some(TextMatchTier::Joined));
        }
        if confusions {
            let matches: Vec<_> = lines
                .iter()
                .filter(|line| one_confusion(&normalized(&line.text, case_sensitive), &query))
                .map(|line| candidate(line))
                .collect();
            if !matches.is_empty() {
                return (matches, Some(TextMatchTier::OcrConfusion));
            }
        }
    }
    (Vec::new(), None)
}

/// Bounded discovery only. The first nonempty tier wins, including ambiguous tiers.
pub fn discover_text(
    lines: &[OcrLine],
    region: &OcrRegionRequest,
    query: &str,
    case_sensitive: bool,
    mode: TextMatchMode,
    confusions: bool,
    limit: u32,
) -> Result<OcrMatchDetails, PlatformError> {
    validate(query, mode, confusions)?;
    if !(1..=100).contains(&limit) {
        return Err(PlatformError::InvalidArgument {
            argument: "max_results".into(),
            reason: "must be between 1 and 100".into(),
        });
    }
    let (matches, tier) = matching(lines, region, query, case_sensitive, mode, confusions);
    Ok(bounded(&matches, tier, limit as usize))
}

/// Mutation selection deliberately supports only exact and substring modes.
pub fn select_text_candidate(
    lines: &[OcrLine],
    region: &OcrRegionRequest,
    query: &str,
    case_sensitive: bool,
    exact_match: bool,
) -> Result<TextMatch, PlatformError> {
    let mode = if exact_match {
        TextMatchMode::Exact
    } else {
        TextMatchMode::Substring
    };
    validate(query, mode, false)?;
    let (mut matches, tier) = matching(lines, region, query, case_sensitive, mode, false);
    match matches.len() {
        0 => {
            let recognized: Vec<_> = lines
                .iter()
                .filter(|line| in_region(line, region))
                .map(candidate)
                .collect();
            Err(PlatformError::OcrNoMatch {
                details: bounded(&recognized, None, CANDIDATE_LIMIT),
            })
        }
        1 => Ok(matches.remove(0)),
        _ => Err(PlatformError::OcrAmbiguousMatch {
            details: bounded(&matches, tier, CANDIDATE_LIMIT),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region() -> OcrRegionRequest {
        OcrRegionRequest {
            display_id: "synthetic".into(),
            x: 10,
            y: 10,
            width: 400,
            height: 400,
            language: None,
        }
    }
    fn line(text: &str, x: u32, y: u32) -> OcrLine {
        OcrLine {
            text: text.into(),
            x,
            y,
            width: 100,
            height: 20,
            words: Vec::new(),
        }
    }
    fn discover(lines: &[OcrLine], query: &str, confusions: bool) -> OcrMatchDetails {
        discover_text(
            lines,
            &region(),
            query,
            false,
            TextMatchMode::Tolerant,
            confusions,
            20,
        )
        .unwrap()
    }

    #[test]
    fn ladder_preserves_strict_results_and_ambiguity() {
        let lines = [line("Save", 10, 10), line("Ｓａｖｅ", 10, 40)];
        let result = discover(&lines, " save ", false);
        assert_eq!(result.match_tier, Some(TextMatchTier::Exact));
        assert_eq!(result.candidate_count, 1);
        let lines = [
            line("Save", 10, 10),
            line("SAVE", 10, 40),
            line("Ｓａｖｅ", 10, 70),
        ];
        let result = discover(&lines, "save", false);
        assert_eq!(result.candidate_count, 2);
        assert_eq!(result.match_tier, Some(TextMatchTier::Exact));
        let result = discover_text(
            &lines,
            &region(),
            "Save",
            true,
            TextMatchMode::Exact,
            false,
            20,
        )
        .unwrap();
        assert_eq!(result.candidate_count, 1);
    }

    #[test]
    fn normalization_is_explicit_and_confusions_are_bounded() {
        for (text, query) in [
            ("Cafe\u{301}", "Café"),
            ("Save\u{a0}\t changes", "Save changes"),
            ("‘Save’—now…", "'Save'-now..."),
            ("Ｓａｖｅ", "Save"),
        ] {
            let lines = [line(text, 10, 10)];
            assert_eq!(
                discover(&lines, query, false).match_tier,
                Some(TextMatchTier::Normalized)
            );
            assert!(select_text_candidate(&lines, &region(), query, false, true).is_err());
        }
        for (text, query, found) in [
            ("C0de", "Code", true),
            ("C00l", "Cool", false),
            ("0n", "On", false),
            ("Savo", "Save", false),
        ] {
            let lines = [line(text, 10, 10)];
            assert_eq!(discover(&lines, query, true).candidate_count == 1, found);
            assert_eq!(discover(&lines, query, false).candidate_count, 0);
        }
        let long = format!("C0{}", "a".repeat(63));
        assert!(!one_confusion(&long, &long.replace('0', "o")));
        assert!(
            discover_text(
                &[],
                &region(),
                "Code",
                false,
                TextMatchMode::Exact,
                true,
                20
            )
            .is_err()
        );
        assert!(
            discover_text(
                &[],
                &region(),
                "  ",
                false,
                TextMatchMode::Tolerant,
                false,
                20
            )
            .is_err()
        );
    }

    #[test]
    fn joins_are_adjacent_bounded_and_discovery_only() {
        let lines = [line("Save", 10, 10), line("changes", 10, 35)];
        let result = discover(&lines, "Save changes", false);
        assert_eq!(result.match_tier, Some(TextMatchTier::Joined));
        assert_eq!(
            (result.candidates[0].y, result.candidates[0].height),
            (10, 45)
        );
        assert!(select_text_candidate(&lines, &region(), "Save changes", false, true).is_err());
        for (x, y) in [(200, 35), (10, 100), (10, 20)] {
            assert_eq!(
                discover(
                    &[line("Save", 10, 10), line("changes", x, y)],
                    "Save changes",
                    false
                )
                .candidate_count,
                0
            );
        }
        let mut reversed = lines.to_vec();
        reversed.reverse();
        assert_eq!(discover(&reversed, "Save changes", false), result);
        let duplicates = [
            line("Save", 10, 10),
            line("changes", 10, 35),
            line("Save", 10, 80),
            line("changes", 10, 105),
        ];
        assert_eq!(
            discover(&duplicates, "Save changes", false).candidate_count,
            2
        );
    }

    #[test]
    fn selection_excludes_out_of_region_and_never_deduplicates_overlaps() {
        let outside = [
            line("private", 0, 10),
            line("private", 350, 10),
            line("private", 10, 405),
            line("private", u32::MAX, 10),
        ];
        let Err(PlatformError::OcrNoMatch { details }) =
            select_text_candidate(&outside, &region(), "private", false, true)
        else {
            panic!("expected no match")
        };
        assert!(details.candidates.is_empty());
        let mut lines = outside.to_vec();
        lines.push(line("Save", 10, 10));
        assert_eq!(
            select_text_candidate(&lines, &region(), "Save", false, true)
                .unwrap()
                .x,
            10
        );
        lines.push(line("Save", 15, 10));
        assert!(matches!(
            select_text_candidate(&lines, &region(), "Save", false, true),
            Err(PlatformError::OcrAmbiguousMatch { .. })
        ));
        lines[5] = lines[4].clone();
        assert!(matches!(
            select_text_candidate(&lines, &region(), "Save", false, true),
            Err(PlatformError::OcrAmbiguousMatch { .. })
        ));
    }

    #[test]
    fn bounded_metadata_does_not_bound_uniqueness_and_truncates_unicode_safely() {
        let text = "é".repeat(300);
        let lines = vec![line(&text, 10, 10); 25];
        for query in [&text, "absent"] {
            let error = select_text_candidate(&lines, &region(), query, false, true).unwrap_err();
            let (PlatformError::OcrNoMatch { details }
            | PlatformError::OcrAmbiguousMatch { details }) = error
            else {
                panic!("wrong error")
            };
            assert_eq!(details.candidate_count, 25);
            assert_eq!(details.candidates.len(), 20);
            assert!(details.candidates_truncated && details.text_truncated);
            assert_eq!(details.candidates[0].text.chars().count(), 256);
        }
        let result = discover_text(
            &lines,
            &region(),
            &text,
            false,
            TextMatchMode::Exact,
            false,
            1,
        )
        .unwrap();
        assert_eq!(result.candidate_count, 25);
        assert_eq!(result.candidates.len(), 1);
        assert!(result.candidates_truncated);
        assert_eq!(discover(&lines, "absent", false).candidate_count, 0);
    }
}
