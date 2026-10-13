//! IRS instructions and publications (irs.gov HTML editions) -> DocChunks,
//! one section per heading, cited by document title and heading.

use anyhow::{Context, Result};
use nii_core::{DocChunk, SourceKind};
use scraper::{ElementRef, Html, Node, Selector};
use std::path::{Path, PathBuf};

use crate::chunk::chunk_section_text;
use crate::uslm::normalize_ws;

/// The guidance documents indexed, by irs.gov path. irs.gov serves the current edition only.
pub const DOCUMENTS: &[&str] = &[
    "instructions/i1040gi",
    "instructions/i1040sd",
    "instructions/i8949",
    "instructions/i2441",
    "instructions/i1040s8",
    "instructions/i1040sc",
    "instructions/i1040se",
    "instructions/i1040sse",
    "instructions/i8995",
    "publications/p17",
    "publications/p501",
    "publications/p503",
    "publications/p550",
    "publications/p334",
    "publications/p527",
    "publications/p925",
];

const SKIPPED: &[&str] = &["script", "style", "noscript", "nav", "button", "svg", "img"];
const SKIPPED_CLASSES: &[&str] = &["index", "indexdiv", "sectioninfo"];
const BLOCKS: &[&str] = &[
    "p",
    "div",
    "li",
    "tr",
    "table",
    "dd",
    "dt",
    "pre",
    "blockquote",
    "caption",
    "br",
    "ul",
    "ol",
    "section",
];

pub struct Guidance {
    pub path: &'static str,
    pub html_path: PathBuf,
}

/// Download (or reuse cached) HTML for every document in [`DOCUMENTS`].
pub fn fetch_guidance(data_dir: &Path, force: bool) -> Result<Vec<Guidance>> {
    let dir = data_dir.join("corpus").join("irs");
    std::fs::create_dir_all(&dir)?;
    DOCUMENTS
        .iter()
        .map(|path| {
            let html_path = dir.join(format!("{}.html", path.rsplit('/').next().unwrap_or(path)));
            if force || !html_path.exists() {
                let url = format!("https://www.irs.gov/{path}");
                let html = crate::download::get_bytes(&url).with_context(|| format!("downloading {url}"))?;
                std::fs::write(&html_path, html)?;
                let fetched = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs();
                std::fs::write(html_path.with_extension("fetched-at"), fetched.to_string())?;
            }
            Ok(Guidance { path, html_path })
        })
        .collect()
}

pub fn guidance_chunks(guidance: &Guidance) -> Result<Vec<DocChunk>> {
    let html = std::fs::read_to_string(&guidance.html_path)
        .with_context(|| format!("reading {}", guidance.html_path.display()))?;
    let source = if guidance.path.starts_with("publications/") {
        SourceKind::IrsPub
    } else {
        SourceKind::IrsInstruction
    };
    let mut chunks = parse(&html, &format!("https://www.irs.gov/{}", guidance.path), source)?;
    let fetched = std::fs::read_to_string(guidance.html_path.with_extension("fetched-at"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());
    for chunk in &mut chunks {
        let timestamp = fetched.map_or_else(
            || "Source download time unknown (legacy cache).".to_string(),
            |t| format!("Source downloaded at Unix time {t}."),
        );
        chunk.caveat = Some(format!("{} {timestamp}", chunk.caveat.as_deref().unwrap_or_default()));
    }
    Ok(chunks)
}

struct Heading {
    level: u8,
    text: String,
    anchor: Option<String>,
}

#[derive(Default)]
struct Walker {
    stack: Vec<Heading>,
    sections: Vec<(Vec<String>, Option<String>, String)>,
    text: String,
}

impl Walker {
    fn flush(&mut self) {
        let text = std::mem::take(&mut self.text);
        if text.split_whitespace().next().is_none() {
            return;
        }
        let path = self.stack.iter().map(|h| h.text.clone()).collect();
        let anchor = self.stack.iter().rev().find_map(|h| h.anchor.clone());
        self.sections.push((path, anchor, text));
    }

    fn walk(&mut self, element: ElementRef) {
        let name = element.value().name();
        if SKIPPED.contains(&name) || element.value().classes().any(|c| SKIPPED_CLASSES.contains(&c)) {
            return;
        }
        if let Some(level) = heading_level(name) {
            self.flush();
            let text = normalize_ws(&element.text().collect::<String>());
            let anchor = element
                .select(&Selector::parse("a[name]").expect("valid selector"))
                .find_map(|a| a.value().attr("name").map(str::to_string));
            self.stack.retain(|h| h.level < level);
            if !text.is_empty() {
                self.stack.push(Heading { level, text, anchor });
            }
            return;
        }
        for child in element.children() {
            match child.value() {
                Node::Text(t) => self.text.push_str(t),
                Node::Element(_) => {
                    if let Some(child) = ElementRef::wrap(child) {
                        self.walk(child);
                    }
                }
                _ => {}
            }
        }
        match name {
            "td" | "th" => self.text.push_str(" | "),
            n if BLOCKS.contains(&n) => self.text.push('\n'),
            _ => {}
        }
    }
}

fn heading_level(name: &str) -> Option<u8> {
    match name.as_bytes() {
        [b'h', d @ b'1'..=b'6'] => Some(d - b'0'),
        _ => None,
    }
}

pub fn parse(html: &str, url: &str, source: SourceKind) -> Result<Vec<DocChunk>> {
    let doc = Html::parse_document(html);
    let title = doc
        .select(&Selector::parse("title").expect("valid selector"))
        .next()
        .map(|t| normalize_ws(&t.text().collect::<String>()))
        .and_then(|t| t.split(" | ").next().map(str::to_string))
        .map(|t| {
            if url.contains("/instructions/") && !t.starts_with("Instructions") {
                format!("Instructions for Form {t}")
            } else {
                t
            }
        })
        .context("page has no <title>")?;
    let root = ["div.book", "article", "body"]
        .iter()
        .find_map(|sel| doc.select(&Selector::parse(sel).expect("valid selector")).next())
        .context("page has no content")?;
    // IRS HTML URLs roll forward. Record the edition actually parsed rather
    // than implying that a current URL is guidance for the requested tax year.
    let edition = regex::Regex::new(r"\((20\d{2})\)")?
        .captures(&title)
        .map(|m| m[1].to_string());
    let caveat = match edition {
        Some(year) => format!("IRS guidance edition: {year}. Use only for the matching tax year; this URL serves the current edition."),
        None => "IRS guidance edition unknown. Verify the tax year before relying on amounts or rules; this URL serves the current edition.".into(),
    };

    let mut walker = Walker::default();
    walker.walk(root);
    walker.flush();

    let mut chunks = Vec::new();
    for (path, anchor, text) in walker.sections {
        let heading = path.last().cloned().unwrap_or_else(|| title.clone());
        let section_path = path[..path.len().saturating_sub(1)].join(" › ");
        let url = match anchor {
            Some(a) => format!("{url}#{a}"),
            None => url.to_string(),
        };
        for (i, text) in chunk_section_text(&tidy_cells(&normalize_ws(&text)))
            .into_iter()
            .enumerate()
        {
            chunks.push(DocChunk {
                source,
                citation: title.clone(),
                section_path: section_path.clone(),
                heading: heading.clone(),
                ordinal: i as u32,
                url: Some(url.clone()),
                text,
                caveat: Some(caveat.clone()),
            });
        }
    }
    Ok(chunks)
}

/// Collapse the separators left by empty table cells.
fn tidy_cells(s: &str) -> String {
    let pipes = regex::Regex::new(r"(?:\s*\|){2,}\s*").expect("valid regex");
    pipes
        .replace_all(s, " | ")
        .trim_matches(|c: char| c == '|' || c.is_whitespace())
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r##"<html><head><title>1040 (2025) | Internal Revenue Service</title></head><body>
<nav><a>Main navigation</a></nav>
<article><div class="col-md-4"><ul><li><a href="#x">Table of contents</a></li></ul></div>
<div class="book">
 <div class="section"><div class="sectioninfo"><strong></strong></div>
  <h2 class="title"><a name="id4"></a>Line 16</h2>
  <p>Figure your tax.</p>
  <h3><a name="id9"></a>Qualified Dividends and Capital Gain Tax Worksheet—Line 16</h3>
  <table><tr><td></td><td>6.</td><td></td><td>Enter: $64,750 if head of household</td></tr>
         <tr><td>7.</td><td>Enter the smaller of line 1 or line 6</td></tr></table>
  <h2><a name="id20"></a>Line 19</h2>
  <ul><li><p>The maximum credit is $2,200.</p></li></ul>
 </div>
 <div class="index"><p>Index: tax, 16</p></div>
</div></article></body></html>"##;

    #[test]
    fn cached_guidance_is_reused_and_chunked_by_source() {
        let dir = std::env::temp_dir().join(format!("nii-guidance-cache-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("corpus/irs")).unwrap();
        for path in DOCUMENTS {
            let name = path.rsplit('/').next().unwrap();
            std::fs::write(dir.join(format!("corpus/irs/{name}.html")), PAGE).unwrap();
        }
        let docs = fetch_guidance(&dir, false).unwrap();
        assert_eq!(docs.len(), DOCUMENTS.len());
        let instructions = guidance_chunks(&docs[0]).unwrap();
        assert_eq!(instructions[0].source, SourceKind::IrsInstruction);
        assert_eq!(
            instructions[0].url.as_deref(),
            Some("https://www.irs.gov/instructions/i1040gi#id4")
        );
        let publication = guidance_chunks(docs.iter().find(|d| d.path == "publications/p17").unwrap()).unwrap();
        assert_eq!(publication[0].source, SourceKind::IrsPub);
        assert_eq!(publication[0].citation, "1040 (2025)");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pages_without_a_title_or_text_are_handled() {
        assert!(parse("<html><body><p>x</p></body></html>", "u", SourceKind::IrsPub).is_err());
        let chunks = parse(
            "<html><head><title>T</title></head><body><h2></h2><p>body</p></body></html>",
            "u",
            SourceKind::IrsPub,
        )
        .unwrap();
        assert_eq!((chunks[0].heading.as_str(), chunks[0].url.as_deref()), ("T", Some("u")));
        assert!(chunks[0].caveat.as_deref().unwrap().contains("edition unknown"));
    }

    #[test]
    fn sections_follow_headings_with_anchors_and_tables() {
        let chunks = parse(
            PAGE,
            "https://www.irs.gov/instructions/i1040gi",
            SourceKind::IrsInstruction,
        )
        .unwrap();
        let view: Vec<(&str, &str, &str, &str)> = chunks
            .iter()
            .map(|c| {
                (
                    c.heading.as_str(),
                    c.section_path.as_str(),
                    c.url.as_deref().unwrap(),
                    c.text.as_str(),
                )
            })
            .collect();
        assert_eq!(
            view,
            vec![
                (
                    "Line 16",
                    "",
                    "https://www.irs.gov/instructions/i1040gi#id4",
                    "Figure your tax."
                ),
                (
                    "Qualified Dividends and Capital Gain Tax Worksheet—Line 16",
                    "Line 16",
                    "https://www.irs.gov/instructions/i1040gi#id9",
                    "6. | Enter: $64,750 if head of household | 7. | Enter the smaller of line 1 or line 6"
                ),
                (
                    "Line 19",
                    "",
                    "https://www.irs.gov/instructions/i1040gi#id20",
                    "The maximum credit is $2,200."
                ),
            ]
        );
        assert!(chunks.iter().all(|c| c.citation == "Instructions for Form 1040 (2025)"));
        assert!(
            chunks
                .iter()
                .all(|c| c.caveat.as_deref().unwrap().contains("edition: 2025"))
        );
    }

    #[test]
    fn guidance_preserves_download_provenance_and_flags_legacy_caches() {
        let dir = nii_testkit::scratch("guidance-provenance");
        let html_path = dir.join("i1040gi.html");
        std::fs::write(&html_path, PAGE).unwrap();
        let guidance = Guidance {
            path: "instructions/i1040gi",
            html_path: html_path.clone(),
        };
        let legacy = guidance_chunks(&guidance).unwrap();
        assert!(legacy[0].caveat.as_deref().unwrap().contains("download time unknown"));
        std::fs::write(html_path.with_extension("fetched-at"), "1760000000").unwrap();
        let chunks = guidance_chunks(&guidance).unwrap();
        let caveat = chunks[0].caveat.as_deref().unwrap();
        assert!(caveat.contains("edition: 2025") && caveat.contains("1760000000"));
    }
}
