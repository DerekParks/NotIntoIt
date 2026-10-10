//! IRS instructions and publications (irs.gov HTML editions) -> DocChunks,
//! one section per heading, cited by document title and heading.

use anyhow::{Context, Result};
use nii_core::{DocChunk, SourceKind};
use scraper::{ElementRef, Html, Node, Selector};
use std::path::{Path, PathBuf};

use crate::chunk::chunk_section_text;

/// The guidance documents indexed, by irs.gov path. irs.gov serves the current edition only.
pub const DOCUMENTS: &[&str] = &[
    "instructions/i1040gi",
    "instructions/i1040sd",
    "instructions/i8949",
    "instructions/i2441",
    "instructions/i1040s8",
    "publications/p17",
    "publications/p501",
    "publications/p503",
    "publications/p550",
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
    parse(&html, &format!("https://www.irs.gov/{}", guidance.path), source)
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
                caveat: None,
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

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
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
    }
}
