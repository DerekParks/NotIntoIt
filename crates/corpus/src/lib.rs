//! Corpus acquisition + parsing + chunking: Title 26 USLM XML and IRS instructions/publications HTML.

pub mod chunk;
pub mod download;
pub mod irs_html;
pub mod uslm;

pub use chunk::{chunk_section_text, section_to_chunks};
pub use uslm::{Amendment, Section, parse_usc};
