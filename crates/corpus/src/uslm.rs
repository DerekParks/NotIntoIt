//! Streaming parser for USLM XML (uscode.house.gov schema).
//!
//! Walks the document once, tracking the container hierarchy (subtitle >
//! chapter > subchapter > part > subpart) and emitting one [`Section`] per
//! non-repealed `<section>`, with editorial apparatus (source credits, notes,
//! tables of contents) excluded from the text. Amendment and effective-date
//! notes are kept separately, so readers can tell when the text took effect.

use anyhow::{Context, Result};
use quick_xml::Reader;
use quick_xml::events::{BytesRef, BytesStart, Event};
use std::io::BufRead;
use std::path::Path;

/// Structural elements above `section` whose num+heading form the section path.
const CONTAINERS: &[&str] = &["subtitle", "chapter", "subchapter", "part", "subpart"];
/// Elements whose entire content is excluded from section text.
const EXCLUDED: &[&str] = &["sourceCredit", "notes", "note", "toc", "meta", "sidenote"];
/// Elements that sit inside running text; closing one must not insert a space.
const INLINE: &[&str] = &[
    "ref",
    "date",
    "i",
    "b",
    "inline",
    "sup",
    "sub",
    "span",
    "term",
    "shortTitle",
    "quotedText",
];
const AMENDMENTS_TOPIC: &str = "amendments";
const EFFECTIVE_TOPIC: &str = "effectiveDateOfAmendment";

#[derive(Debug, Clone)]
pub struct Section {
    /// USLM identifier, e.g. "/us/usc/t26/s61".
    pub identifier: String,
    /// Section number as printed, e.g. "61" (from `<num value="61">`).
    pub num: String,
    /// Section heading, e.g. "Gross income defined".
    pub heading: String,
    /// Joined container path, e.g. "Subtitle A—Income Taxes › CHAPTER 1—...".
    pub path: String,
    /// Whitespace-normalized body text (subsection nums inline).
    pub text: String,
    /// Amendment notes, newest first as printed: one entry per dated change.
    pub amendments: Vec<Amendment>,
    /// "Effective Date of YYYY Amendment" notes.
    pub effective_dates: Vec<Amendment>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Amendment {
    pub year: u16,
    pub text: String,
}

struct Container {
    label: String,
}

#[derive(Default)]
struct PendingSection {
    identifier: String,
    num_value: String,
    heading: String,
    text: String,
    repealed: bool,
    amendments: Vec<Amendment>,
    effective_dates: Vec<Amendment>,
}

/// A note being captured: its element depth, topic, heading, and paragraphs.
struct NoteCapture {
    depth: usize,
    topic: String,
    heading: String,
    paragraphs: Vec<String>,
    current: String,
}

impl NoteCapture {
    fn flush(&mut self) {
        let p = normalize_ws(&std::mem::take(&mut self.current));
        if !p.is_empty() {
            self.paragraphs.push(p);
        }
    }

    fn finish(mut self, into: &mut PendingSection) {
        self.flush();
        if self.topic == AMENDMENTS_TOPIC {
            let mut year = None;
            for p in self.paragraphs {
                let (y, body) = split_year(&p);
                year = y.or(year);
                if let Some(year) = year {
                    into.amendments.push(Amendment {
                        year,
                        text: body.to_string(),
                    });
                }
            }
        } else if let Some(year) = heading_year(&self.heading) {
            into.effective_dates.push(Amendment {
                year,
                text: self.paragraphs.join(" "),
            });
        }
    }
}

/// "2025—Subsec. (a)..." -> (Some(2025), "Subsec. (a)...").
fn split_year(p: &str) -> (Option<u16>, &str) {
    match p.split_once('—') {
        Some((y, rest)) if y.len() == 4 => match y.parse() {
            Ok(year) => (Some(year), rest.trim()),
            Err(_) => (None, p),
        },
        _ => (None, p),
    }
}

/// "Effective Date of 2025 Amendment" -> 2025.
fn heading_year(heading: &str) -> Option<u16> {
    heading
        .split_whitespace()
        .find_map(|w| w.parse().ok().filter(|y| (1900..2200).contains(y)))
}

pub fn parse_usc(xml_path: &Path) -> Result<Vec<Section>> {
    let file = std::fs::File::open(xml_path).with_context(|| format!("opening {}", xml_path.display()))?;
    parse_usc_reader(std::io::BufReader::new(file))
}

pub fn parse_usc_reader<R: BufRead>(input: R) -> Result<Vec<Section>> {
    let mut reader = Reader::from_reader(input);
    let mut buf = Vec::new();

    let mut sections = Vec::new();
    let mut elem_stack: Vec<String> = Vec::new();
    let mut containers: Vec<Container> = Vec::new();
    let mut excluded_from_depth: Option<usize> = None;
    let mut section: Option<PendingSection> = None;
    let mut note: Option<NoteCapture> = None;

    loop {
        // Text arrives as Text events plus GeneralRef events for entities
        // (quick-xml 0.41 no longer inlines them); both feed one sink below.
        let piece: Option<String> = match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                let name = local_name(&e);
                elem_stack.push(name.clone());

                if name == "note" && section.is_some() && note.is_none() {
                    let topic = attr(&e, "topic").unwrap_or_default();
                    if topic == AMENDMENTS_TOPIC || topic == EFFECTIVE_TOPIC {
                        note = Some(NoteCapture {
                            depth: elem_stack.len(),
                            topic,
                            heading: String::new(),
                            paragraphs: Vec::new(),
                            current: String::new(),
                        });
                    }
                }

                if excluded_from_depth.is_none() && EXCLUDED.contains(&name.as_str()) {
                    excluded_from_depth = Some(elem_stack.len());
                }
                if excluded_from_depth.is_none() {
                    if name == "section" && section.is_none() {
                        section = Some(PendingSection {
                            identifier: attr(&e, "identifier").unwrap_or_default(),
                            repealed: attr(&e, "status").as_deref() == Some("repealed"),
                            ..Default::default()
                        });
                    } else if section.is_none() && CONTAINERS.contains(&name.as_str()) {
                        containers.push(Container { label: String::new() });
                    } else if name == "num" && section.is_some() && parent_is(&elem_stack, "section") {
                        // Prefer the clean @value ("61") over printed text ("§ 61.").
                        if let (Some(s), Some(v)) = (section.as_mut(), attr(&e, "value")) {
                            s.num_value = v;
                        }
                    }
                }
                None
            }
            Event::End(_) => {
                let name = elem_stack.pop().unwrap_or_default();
                if let Some(n) = note.as_mut() {
                    if elem_stack.len() + 1 == n.depth {
                        if let (Some(n), Some(s)) = (note.take(), section.as_mut()) {
                            n.finish(s);
                        }
                    } else if name == "p" && elem_stack.len() == n.depth {
                        n.flush();
                    } else if !INLINE.contains(&name.as_str()) {
                        n.current.push(' ');
                    }
                }
                if excluded_from_depth == Some(elem_stack.len() + 1) {
                    excluded_from_depth = None;
                } else if excluded_from_depth.is_none() {
                    match name.as_str() {
                        "section" => {
                            if let Some(s) = section.take()
                                && !s.repealed
                                && !s.text.trim().is_empty()
                            {
                                sections.push(Section {
                                    identifier: s.identifier,
                                    num: s.num_value,
                                    heading: normalize_ws(&s.heading),
                                    path: containers
                                        .iter()
                                        .map(|c| normalize_ws(&c.label))
                                        .filter(|l| !l.is_empty())
                                        .collect::<Vec<_>>()
                                        .join(" › "),
                                    text: normalize_ws(&s.text),
                                    amendments: s.amendments,
                                    effective_dates: s.effective_dates,
                                });
                            }
                        }
                        n if section.is_none() && CONTAINERS.contains(&n) => {
                            containers.pop();
                        }
                        n if section.is_some() && !INLINE.contains(&n) => {
                            // Block boundary: keep words from fusing across elements.
                            if let Some(s) = section.as_mut() {
                                s.text.push(' ');
                            }
                        }
                        _ => {}
                    }
                }
                None
            }
            Event::Text(t) => Some(t.xml10_content()?.into_owned()),
            Event::GeneralRef(r) => Some(resolve_ref(&r)?),
            Event::Eof => break,
            _ => None,
        };

        if let Some(text) = piece {
            if let Some(n) = note.as_mut() {
                if elem_stack.last().map(String::as_str) == Some("heading") && elem_stack.len() == n.depth + 1 {
                    n.heading.push_str(&text);
                } else {
                    n.current.push_str(&text);
                }
            } else if excluded_from_depth.is_some() {
                // skip
            } else if let Some(s) = section.as_mut() {
                match (elem_stack.last().map(String::as_str), parent_is(&elem_stack, "section")) {
                    (Some("heading"), true) => s.heading.push_str(&text),
                    (Some("num"), true) => {} // printed form; @value already captured
                    _ => s.text.push_str(&text),
                }
            } else if let Some(c) = containers.last_mut() {
                // Only direct num/heading children of the container itself.
                if matches!(elem_stack.last().map(String::as_str), Some("num") | Some("heading"))
                    && elem_stack
                        .get(elem_stack.len().wrapping_sub(2))
                        .is_some_and(|p| CONTAINERS.contains(&p.as_str()))
                {
                    c.label.push_str(&text);
                }
            }
        }
        buf.clear();
    }
    Ok(sections)
}

fn resolve_ref(r: &BytesRef) -> Result<String> {
    if let Some(c) = r.resolve_char_ref()? {
        return Ok(c.to_string());
    }
    Ok(match r.decode()?.as_ref() {
        "amp" => "&".to_string(),
        "lt" => "<".to_string(),
        "gt" => ">".to_string(),
        "apos" => "'".to_string(),
        "quot" => "\"".to_string(),
        other => format!("&{other};"), // unknown entity: keep visible rather than drop
    })
}

fn local_name(e: &BytesStart) -> String {
    String::from_utf8_lossy(e.local_name().as_ref()).into_owned()
}

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.try_get_attribute(name)
        .ok()
        .flatten()
        .and_then(|a| a.normalized_value(quick_xml::XmlVersion::Explicit1_0).ok())
        .map(|v| v.into_owned())
}

fn parent_is(stack: &[String], name: &str) -> bool {
    stack.len() >= 2 && stack[stack.len() - 2] == name
}

pub fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<uscDoc xmlns="http://xml.house.gov/schemas/uslm/1.0">
 <meta><docTitle>skip me</docTitle></meta>
 <main>
  <title identifier="/us/usc/t26">
   <num value="26">Title 26—</num><heading>INTERNAL REVENUE CODE</heading>
   <subtitle identifier="/us/usc/t26/stA">
    <num value="A">Subtitle A—</num><heading>Income Taxes</heading>
    <chapter identifier="/us/usc/t26/stA/ch1">
     <num value="1">CHAPTER 1—</num><heading>NORMAL TAXES AND SURTAXES</heading>
     <toc><tocItem>skip this too</tocItem></toc>
     <section identifier="/us/usc/t26/s61">
      <num value="61">&#xA7; 61.</num>
      <heading> Gross income defined</heading>
      <subsection identifier="/us/usc/t26/s61/a">
       <num value="a">(a)</num><heading> General definition</heading>
       <chapeau>Except as otherwise provided in this subtitle, gross income means all income from whatever source derived, including (but not limited to) the following items:</chapeau>
       <paragraph identifier="/us/usc/t26/s61/a/1">
        <num value="1">(1)</num>
        <content>Compensation for services, including fees, commissions, fringe benefits, and similar items; see &#xA7; 132 &amp; related.</content>
       </paragraph>
      </subsection>
      <sourceCredit>(Aug. 16, 1954, ch. 736, 68A Stat. 17.)</sourceCredit>
      <notes type="uscNote">
       <note topic="amendments"><heading>Amendments</heading><p>2025—Subsec. (a). <ref>Pub. L. 119–21</ref> substituted “$7,500” for “$5,000”.</p>
        <p>Subsec. (b). <ref>Pub. L. 119–21</ref> added par. (2).</p>
        <p>1984—Pub. L. 98-369 struck out stuff.</p></note>
       <note topic="effectiveDateOfAmendment"><heading>Effective Date of 2025 Amendment</heading><p>Pub. L. 119–21 provided that: <quotedContent>“The amendment shall apply to taxable years beginning after <date>December 31, 2025</date>.”</quotedContent></p></note>
       <note topic="miscellaneous"><heading>Regulations</heading><p>skip regs</p></note>
      </notes>
     </section>
     <section identifier="/us/usc/t26/s62" status="repealed">
      <num value="62">&#xA7; 62.</num>
      <heading> Repealed.</heading>
      <content>Repealed text.</content>
     </section>
    </chapter>
   </subtitle>
  </title>
 </main>
</uscDoc>"#;

    #[test]
    fn parses_sections_with_path_and_exclusions() {
        let sections = parse_usc_reader(SAMPLE.as_bytes()).unwrap();
        assert_eq!(sections.len(), 1, "repealed section must be skipped");
        let s = &sections[0];
        assert_eq!(s.identifier, "/us/usc/t26/s61");
        assert_eq!(s.num, "61");
        assert_eq!(s.heading, "Gross income defined");
        assert_eq!(s.path, "Subtitle A—Income Taxes › CHAPTER 1—NORMAL TAXES AND SURTAXES");
        assert!(
            s.text
                .starts_with("(a) General definition Except as otherwise provided"),
            "text: {}",
            s.text
        );
        assert!(s.text.contains("(1) Compensation for services"));
        assert!(s.text.contains("see § 132 & related"), "entities: {}", s.text);
        assert!(!s.text.contains("68A Stat"), "sourceCredit leaked: {}", s.text);
        assert!(!s.text.contains("Pub. L. 98-369"), "notes leaked");
        assert!(!s.text.contains("skip"), "meta/toc leaked");
        assert_eq!(
            s.amendments,
            vec![
                Amendment {
                    year: 2025,
                    text: "Subsec. (a). Pub. L. 119–21 substituted “$7,500” for “$5,000”.".into()
                },
                Amendment {
                    year: 2025,
                    text: "Subsec. (b). Pub. L. 119–21 added par. (2).".into()
                },
                Amendment {
                    year: 1984,
                    text: "Pub. L. 98-369 struck out stuff.".into()
                },
            ]
        );
        assert_eq!(s.effective_dates.len(), 1);
        assert_eq!(s.effective_dates[0].year, 2025);
        assert!(
            s.effective_dates[0]
                .text
                .ends_with("taxable years beginning after December 31, 2025.”"),
            "{:?}",
            s.effective_dates
        );
    }
}
