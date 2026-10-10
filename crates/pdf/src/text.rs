//! Text layer extraction, page by page.

use anyhow::Result;
use pdfium_render::prelude::*;

pub fn page_texts(pdfium: &Pdfium, pdf: &[u8]) -> Result<Vec<String>> {
    let document = pdfium.load_pdf_from_byte_slice(pdf, None)?;
    document.pages().iter().map(|page| Ok(page.text()?.all())).collect()
}
