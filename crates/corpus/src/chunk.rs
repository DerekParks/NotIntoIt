//! Section text -> DocChunks. Splitting prefers sentence boundaries, falls
//! back to word boundaries; every chunk inherits the section's provenance.

use nii_core::{DocChunk, SourceKind};

#[cfg(test)]
use crate::uslm::Amendment;
use crate::uslm::Section;

/// Aim for ~this many chars per chunk (~300 tokens for legal English).
pub const TARGET_CHARS: usize = 1200;
/// Never exceed this (bge-small truncates at 512 tokens; stay well under).
pub const MAX_CHARS: usize = 1800;

/// Amendments from this year on are indexed as amendment-note chunks.
pub const AMENDMENT_NOTES_SINCE: u16 = 2017;
/// Amendments from this year on put a caveat on every chunk of the section.
pub const CAVEAT_SINCE: u16 = 2022;
const MAX_EFFECTIVE_PHRASES: usize = 3;

pub fn section_to_chunks(section: &Section) -> Vec<DocChunk> {
    let citation = format!("26 U.S.C. § {}", section.num);
    let note_citation = format!("{citation} note");
    let url = format!(
        "https://uscode.house.gov/view.xhtml?req=granuleid:USC-prelim-title26-section{}&num=0&edition=prelim",
        section.num
    );
    let caveat = amendment_caveat(section, &note_citation);
    let body = chunk_section_text(&section.text)
        .into_iter()
        .map(|text| (citation.clone(), section.heading.clone(), text));
    let notes = chunk_section_text(&amendment_notes(section)).into_iter().map(|text| {
        (
            note_citation.clone(),
            format!("Amendments and effective dates — {}", section.heading),
            text,
        )
    });
    body.chain(notes)
        .enumerate()
        .map(|(i, (citation, heading, text))| DocChunk {
            source: SourceKind::Usc,
            citation,
            section_path: section.path.clone(),
            heading,
            ordinal: i as u32,
            url: Some(url.clone()),
            text,
            caveat: caveat.clone(),
        })
        .collect()
}

/// Recent amendments, each year with its effective-date note.
fn amendment_notes(section: &Section) -> String {
    let mut years: Vec<u16> = section
        .amendments
        .iter()
        .map(|a| a.year)
        .filter(|y| *y >= AMENDMENT_NOTES_SINCE)
        .collect();
    years.dedup();
    years
        .iter()
        .map(|year| {
            let changes: Vec<&str> = section
                .amendments
                .iter()
                .filter(|a| a.year == *year)
                .map(|a| a.text.as_str())
                .collect();
            let effective: Vec<&str> = section
                .effective_dates
                .iter()
                .filter(|e| e.year == *year)
                .map(|e| e.text.as_str())
                .collect();
            let mut out = format!("{year} amendments: {}", changes.join(" "));
            if !effective.is_empty() {
                out.push_str(&format!(
                    " Effective date of {year} amendments: {}",
                    effective.join(" ")
                ));
            }
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// "Amended 2025 by Pub. L. 119–21, applying to taxable years beginning after December 31, 2025. …"
fn amendment_caveat(section: &Section, note_citation: &str) -> Option<String> {
    let mut years: Vec<u16> = section
        .amendments
        .iter()
        .map(|a| a.year)
        .filter(|y| *y >= CAVEAT_SINCE)
        .collect();
    years.sort_unstable_by(|a, b| b.cmp(a));
    years.dedup();
    if years.is_empty() {
        return None;
    }
    let laws = regex::Regex::new(r"Pub\. L\. \d+[–-]\d+").expect("valid regex");
    let applies =
        regex::Regex::new(r"(?:[A-Za-z]+ ){1,4}(?:on or )?(?:after|before) [A-Z][a-z]{2,8}\.? \d{1,2}, \d{4}")
            .expect("valid regex");
    let lead = regex::Regex::new(r"^(?:(?:shall|apply|applies|applicable|to|and|the|with|respect|any) )+")
        .expect("valid regex");
    let parts: Vec<String> = years
        .iter()
        .map(|year| {
            let mut law: Vec<&str> = section
                .amendments
                .iter()
                .filter(|a| a.year == *year)
                .filter_map(|a| laws.find(&a.text).map(|m| m.as_str()))
                .collect();
            law.sort_unstable();
            law.dedup();
            let mut when: Vec<String> = section
                .effective_dates
                .iter()
                .filter(|e| e.year == *year)
                .flat_map(|e| applies.find_iter(&e.text).map(|m| m.as_str()))
                .map(|w| full_month(&lead.replace(w, "")))
                .collect();
            when.sort_unstable();
            when.dedup();
            when.truncate(MAX_EFFECTIVE_PHRASES);
            let by = if law.is_empty() {
                String::new()
            } else {
                format!(" by {}", law.join(", "))
            };
            let effective = match when.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
                [] => " (effective date in the amendment notes)".to_string(),
                w => format!(", applying to {}", w.join("; ")),
            };
            format!("Amended {year}{by}{effective}")
        })
        .collect();
    Some(format!(
        "{}. This is the current text; for an earlier tax year, check what changed in \"{note_citation}\".",
        parts.join(". ")
    ))
}

fn full_month(phrase: &str) -> String {
    const MONTHS: [(&str, &str); 9] = [
        ("Jan.", "January"),
        ("Feb.", "February"),
        ("Aug.", "August"),
        ("Sept.", "September"),
        ("Sep.", "September"),
        ("Oct.", "October"),
        ("Nov.", "November"),
        ("Dec.", "December"),
        ("Mar.", "March"),
    ];
    MONTHS
        .iter()
        .fold(phrase.to_string(), |p, (short, long)| p.replace(short, long))
}

/// Split normalized text into chunks of roughly TARGET_CHARS, preferring to
/// break after sentence-ending punctuation, never exceeding MAX_CHARS.
pub fn chunk_section_text(text: &str) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    if text.len() <= MAX_CHARS {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::with_capacity(TARGET_CHARS + 128);
    let mut sentence_break_at: usize = 0; // byte offset in `current` after last sentence end

    for word in text.split(' ') {
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
        if ends_sentence(word) {
            sentence_break_at = current.len();
        }

        if current.len() >= TARGET_CHARS && sentence_break_at > 0 {
            let rest = current.split_off(sentence_break_at);
            chunks.push(std::mem::take(&mut current).trim().to_string());
            current = rest.trim_start().to_string();
            sentence_break_at = 0;
        } else if current.len() >= MAX_CHARS {
            chunks.push(std::mem::take(&mut current).trim().to_string());
            sentence_break_at = 0;
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current.trim().to_string());
    }
    chunks.retain(|c| !c.is_empty());
    chunks
}

fn ends_sentence(word: &str) -> bool {
    // "provided." / "items:" / "follows;" — legal text delimits clauses heavily.
    matches!(word.chars().last(), Some('.') | Some(':') | Some(';'))
        // avoid breaking after enumerators like "(a)." or "1."
        && word.len() > 3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(amendments: Vec<Amendment>, effective_dates: Vec<Amendment>) -> Section {
        Section {
            identifier: "/us/usc/t26/s129".into(),
            num: "129".into(),
            heading: "Dependent care assistance programs".into(),
            path: "Subtitle A".into(),
            text: "The exclusion shall not exceed $7,500.".into(),
            amendments,
            effective_dates,
        }
    }

    #[test]
    fn recent_amendments_get_a_caveat_and_a_notes_chunk() {
        let s = section(
            vec![
                Amendment { year: 2025, text: "Subsec. (a)(2)(A). Pub. L. 119–21 substituted “$7,500” for “$5,000”.".into() },
                Amendment { year: 2004, text: "Subsec. (c)(2). Pub. L. 108–311 substituted text.".into() },
            ],
            vec![Amendment {
                year: 2025,
                text: "Pub. L. 119–21 provided that: “The amendment made by this section shall apply to taxable years beginning after December 31, 2025.”".into(),
            }],
        );
        let chunks = section_to_chunks(&s);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].citation, "26 U.S.C. § 129");
        assert_eq!(
            chunks[0].caveat.as_deref(),
            Some(
                "Amended 2025 by Pub. L. 119–21, applying to taxable years beginning after December 31, 2025. This is the current text; for an earlier tax year, check what changed in \"26 U.S.C. § 129 note\"."
            )
        );
        assert_eq!(chunks[1].citation, "26 U.S.C. § 129 note");
        assert!(
            chunks[1].text.starts_with("2025 amendments: Subsec. (a)(2)(A)"),
            "{}",
            chunks[1].text
        );
        assert!(chunks[1].text.contains("Effective date of 2025 amendments"));
        assert!(!chunks[1].text.contains("2004"), "pre-2017 amendments are not indexed");
    }

    #[test]
    fn old_amendments_get_no_caveat() {
        let s = section(
            vec![Amendment {
                year: 2004,
                text: "Pub. L. 108–311 substituted text.".into(),
            }],
            vec![],
        );
        let chunks = section_to_chunks(&s);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].caveat, None);
    }

    #[test]
    fn short_text_is_one_chunk() {
        assert_eq!(chunk_section_text("hello world."), vec!["hello world."]);
    }

    #[test]
    fn long_text_splits_on_sentence_boundaries() {
        let sentence = "The taxpayer shall include all amounts received in gross income; ";
        let text = sentence.repeat(60); // ~4000 chars
        let chunks = chunk_section_text(&text);
        assert!(chunks.len() >= 3, "got {} chunks", chunks.len());
        for c in &chunks {
            assert!(c.len() <= MAX_CHARS, "chunk too long: {}", c.len());
            assert!(
                c.ends_with(';') || c.ends_with('.') || std::ptr::eq(c, chunks.last().unwrap()),
                "mid-sentence break: ...{}",
                &c[c.len().saturating_sub(30)..]
            );
        }
    }

    #[test]
    fn unbreakable_text_still_respects_max() {
        let text = "word ".repeat(1000); // no sentence enders
        for c in chunk_section_text(&text) {
            assert!(c.len() <= MAX_CHARS + 5);
        }
    }
}
