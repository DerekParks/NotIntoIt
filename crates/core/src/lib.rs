//! Domain types shared across NotIntoIt crates.

pub mod calc;
pub mod tax;

use std::fmt;

/// Which body of law/guidance a chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    /// Title 26 U.S. Code (the Internal Revenue Code).
    Usc,
    /// Treasury Regulations, 26 CFR.
    Cfr,
    /// IRS Publications (Pub 17, 501, ...).
    IrsPub,
    /// IRS form instructions (i1040, ...).
    IrsInstruction,
}

impl SourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceKind::Usc => "usc",
            SourceKind::Cfr => "cfr",
            SourceKind::IrsPub => "irs-pub",
            SourceKind::IrsInstruction => "irs-instr",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "usc" => Some(SourceKind::Usc),
            "cfr" => Some(SourceKind::Cfr),
            "irs-pub" => Some(SourceKind::IrsPub),
            "irs-instr" => Some(SourceKind::IrsInstruction),
            _ => None,
        }
    }
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One indexable chunk of law text, carrying enough provenance to cite it.
#[derive(Debug, Clone)]
pub struct DocChunk {
    pub source: SourceKind,
    /// Human citation, e.g. "26 U.S.C. § 61".
    pub citation: String,
    /// Hierarchy above the section, e.g. "Subtitle A › Ch. 1 › Subch. B › Part I".
    pub section_path: String,
    /// Section heading, e.g. "Gross income defined".
    pub heading: String,
    /// Chunk index within the section (0-based); sections longer than the
    /// chunk target are split.
    pub ordinal: u32,
    /// Link to the authoritative source.
    pub url: Option<String>,
    /// The chunk text itself, whitespace-normalized, no heading prefix.
    pub text: String,
    /// Shown with every hit, e.g. that the text was amended and when that took effect.
    pub caveat: Option<String>,
}

impl DocChunk {
    /// The string that gets embedded: citation+heading prefixed so the vector
    /// carries the section's identity even for mid-section chunks.
    pub fn embed_text(&self) -> String {
        format!("{} — {}: {}", self.citation, self.heading, self.text)
    }
}
