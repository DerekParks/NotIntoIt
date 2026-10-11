//! `nii mcp`: stdio MCP server for IRS form fetch/inspect/fill/view and tax-law search,
//! building the search index in the background. Diagnostics go to stderr.

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{documents, forms, index};

const RENDER_WIDTH: u32 = 1200;
const OTHER_BUILD_POLL: Duration = Duration::from_secs(5);
const RETRY_FIRST: Duration = Duration::from_secs(60);
const RETRY_MAX: Duration = Duration::from_secs(30 * 60);
const FILL_PROBLEMS_SHOWN: usize = 10;
/// Field listings beyond this many bytes are split by page, then cut.
const LISTING_MAX_BYTES: usize = 16 * 1024;
const FILL_ECHO_DESCRIPTION: usize = 90;
/// Description sentences at least this long are shown once per listing.
const REPEATED_SENTENCE_MIN: usize = 60;

const INSTRUCTIONS: &str = "NotIntoIt prepares US federal returns on real IRS PDF forms, with tools for the \
user's documents, tax law, and arithmetic.

Start with list_documents and read_document to see the user's source documents (W-2s, 1099s, notes).

Forms: fetch_irs_form -> list_form_fields -> fill_form -> view_pdf. fetch_irs_form takes names like \
\"1040\", \"Schedule C\", \"8812\", \"W-2\". Use each field's description to decide which line it is, and copy its name \
as listed (e.g. Page1[0].f1_14[0]); never make names up or number them yourself. Fill only lines that have an \
entry and leave the rest blank. Checkboxes take true/false. Example values: {\"Page1[0].f1_14[0]\": \"Alex\", \
\"Page1[0].f1_47[0]\": \"50000\", \"Page1[0].c1_2[0]\": false}. fill_form can be called repeatedly on the same PDF; \
earlier values are kept. It writes every valid value, rejects bad ones individually (resend only those, fixed), \
and lists what landed in which field: check that each value is on the line you meant. view_pdf shows a page and \
its filled values. The return is done when every form it needs has been filled with fill_form; a summary of \
amounts is not a return.

Law: search_tax_law covers Title 26 and the current IRS instructions and publications. Cite what you rely on. \
Statute text is current law: when a hit carries an amendment caveat, check whether the change applies to the tax \
year you are preparing. Prefer the year's IRS instructions for dollar amounts.

Math: never do arithmetic in your head. Use calculate for line arithmetic and compute_tax for Form 1040 line 16. \
This is not tax advice and nothing is e-filed: the user reviews every filled form before using it.";

pub struct Paths {
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub workspace: PathBuf,
}

pub fn serve(paths: Paths) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let server = NiiServer::new(paths);
        server.index.ensure_building(&server.paths);
        let service = server
            .serve(rmcp::transport::stdio())
            .await
            .context("starting MCP server")?;
        service.waiting().await?;
        Ok(())
    })
}

#[derive(Clone)]
pub struct NiiServer {
    paths: Arc<Paths>,
    index: Arc<IndexTracker>,
    query_embedder: Arc<Mutex<Option<nii_embed::Embedder>>>,
    tool_router: ToolRouter<Self>,
}

#[derive(Deserialize, JsonSchema)]
pub struct FetchFormArgs {
    /// IRS form name or file stem: "1040", "W-2", "f1040s1" (Schedule 1), "f1040sb" (Schedule B), "f8949".
    form: String,
    /// Tax year the form is for, e.g. 2025.
    tax_year: u16,
    /// Replace an existing working copy (discarding anything filled in it). Default false.
    #[serde(default)]
    overwrite: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListFieldsArgs {
    /// Path to the PDF (as returned by fetch_irs_form).
    pdf: String,
    /// Only fields on this 1-based page.
    page: Option<u32>,
    /// Case-insensitive substring of field name or description, e.g. "social security". Section headings
    /// usually appear only on a group's first field, so list the whole page to see every box in a group.
    search: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct FillFormArgs {
    /// Path to the PDF to fill (as returned by fetch_irs_form).
    pdf: String,
    /// Field name as listed by list_form_fields (prefix omitted) -> value. Text fields take strings (or
    /// numbers); checkboxes take true/false.
    values: BTreeMap<String, serde_json::Value>,
    /// Write the result here instead of updating `pdf` in place.
    output: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ViewPdfArgs {
    /// Path to the PDF.
    pdf: String,
    /// 1-based page to render. Default 1.
    page: Option<u32>,
    /// Also show the PDF to the user in their PDF viewer. The page image comes back either way. Default false.
    #[serde(default)]
    open: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListDocumentsArgs {
    /// Folder inside the tax workspace, e.g. "2025". Default: the whole workspace.
    folder: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReadDocumentArgs {
    /// File inside the tax workspace (as listed by list_documents): PDF, txt, md, csv, tsv, or json.
    path: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct CalculateArgs {
    /// Expressions to evaluate, e.g. ["72,805 + 665 + 486 + 2,532", "min(5000, 3000)", "round(1184.52)"].
    /// Supports + - * / ( ), percentages like 12%, and min, max, abs, round(x[, digits]).
    expressions: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ComputeTaxArgs {
    /// Tax year, e.g. 2025.
    tax_year: u16,
    /// single, married filing jointly, married filing separately, head of household, or qualifying surviving spouse.
    filing_status: String,
    /// Form 1040 line 15.
    taxable_income: f64,
    /// Form 1040 line 3a. Default 0.
    #[serde(default)]
    qualified_dividends: f64,
    /// Smaller of Schedule D lines 15 and 16 (0 if either is a loss), or Form 1040 line 7a when Schedule D isn't
    /// required. Default 0. Don't use this tool if Schedule D lines 18 or 19 are nonzero.
    #[serde(default)]
    net_capital_gain: f64,
}

#[derive(Deserialize, JsonSchema)]
pub struct SearchArgs {
    /// Natural-language question or citation, e.g. "home office deduction" or "401(k) contribution limits".
    query: String,
    /// Maximum results (1-20). Default 6.
    limit: Option<usize>,
}

#[tool_router]
impl NiiServer {
    fn new(paths: Paths) -> Self {
        Self {
            paths: Arc::new(paths),
            index: Arc::new(IndexTracker::default()),
            query_embedder: Arc::new(Mutex::new(None)),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Download a blank IRS form for a tax year from irs.gov into the user's tax workspace and \
        return its path. Keeps an existing working copy (and its filled values) unless overwrite is true."
    )]
    async fn fetch_irs_form(&self, Parameters(args): Parameters<FetchFormArgs>) -> Result<CallToolResult, McpError> {
        let paths = self.paths.clone();
        blocking(move || {
            let fetched = forms::fetch(
                &paths.data_dir,
                &paths.workspace,
                &args.form,
                args.tax_year,
                args.overwrite,
            )?;
            let fields = nii_pdf::acroform::read_fields(&std::fs::read(&fetched.path)?)?;
            let filled = fields.iter().filter(|f| f.value.is_some()).count();
            let caveat = match fetched.revision {
                Some(r) if r != args.tax_year => format!(
                    " (the {r} revision is the one in effect for {}; fill in the calendar year on the form)",
                    args.tax_year
                ),
                _ => String::new(),
            };
            let status = match (&fetched.downloaded, fetched.created) {
                (Some(d), _) => format!("Downloaded {}{caveat} ->", d.url),
                (None, true) => format!("Created from the cached download{caveat} ->"),
                (None, false) => format!(
                    "Kept existing working copy ({filled} fields already filled; pass overwrite=true to start fresh):"
                ),
            };
            Ok(vec![ContentBlock::text(format!(
                "{status} {}\n{} fillable fields. Next: list_form_fields with pdf={0:?}.",
                fetched.path.display(),
                fields.len(),
            ))])
        })
        .await
    }

    #[tool(
        description = "List a PDF form's fillable fields: name, type (text/checkbox/radio), page, length \
        limit, a description of what the field is for, and current value. Filter by page or search text."
    )]
    async fn list_form_fields(&self, Parameters(args): Parameters<ListFieldsArgs>) -> Result<CallToolResult, McpError> {
        let pdf = self.resolve(&args.pdf);
        blocking(move || {
            let pdf = pdf?;
            let fields = nii_pdf::acroform::read_fields(
                &std::fs::read(&pdf).with_context(|| format!("reading {}", pdf.display()))?,
            )?;
            let total = fields.len();
            let all_names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
            let needle = args.search.as_deref().map(str::to_lowercase);
            let shown: Vec<_> = fields
                .into_iter()
                .filter(|f| args.page.is_none_or(|p| f.page == Some(p)))
                .filter(|f| {
                    needle.as_deref().is_none_or(|n| {
                        f.name.to_lowercase().contains(n)
                            || f.description.as_deref().is_some_and(|d| d.to_lowercase().contains(n))
                    })
                })
                .collect();
            let prefix = nii_pdf::acroform::common_prefix(all_names.iter().map(String::as_str));
            let mut rendered = render_fields(&shown, prefix);
            let mut notes = Vec::new();
            let mut listed = shown.len();
            if rendered.len() > LISTING_MAX_BYTES && args.page.is_none() {
                let first = shown.iter().filter_map(|f| f.page).min();
                let mut others: BTreeMap<u32, usize> = BTreeMap::new();
                for page in shown.iter().filter_map(|f| f.page).filter(|p| Some(*p) != first) {
                    *others.entry(page).or_default() += 1;
                }
                if let (Some(first), false) = (first, others.is_empty()) {
                    let page_fields: Vec<_> = shown
                        .iter()
                        .filter(|f| f.page == Some(first) || f.page.is_none())
                        .cloned()
                        .collect();
                    listed = page_fields.len();
                    rendered = render_fields(&page_fields, prefix);
                    let rest: Vec<String> = others
                        .iter()
                        .map(|(page, n)| format!("page {page} has {n} (list_form_fields with page={page})"))
                        .collect();
                    notes.push(format!("Showing page {first} only; {}.", rest.join("; ")));
                }
            }
            if rendered.len() > LISTING_MAX_BYTES {
                let limit = (0..=LISTING_MAX_BYTES)
                    .rev()
                    .find(|&i| rendered.is_char_boundary(i))
                    .unwrap_or(0);
                let cut = rendered[..limit].rfind('\n').unwrap_or(0);
                let kept = rendered[..cut].lines().count();
                notes.push(format!(
                    "{} more of these fields not shown; narrow the listing with search (e.g. a line number) or page.",
                    listed.saturating_sub(kept)
                ));
                rendered.truncate(cut);
                listed = kept;
            }
            let notes = notes.iter().map(|n| format!("\n{n}")).collect::<String>();
            Ok(vec![ContentBlock::text(format!(
                "{listed} of {total} fields, one per line: name (type, page, limits) description [= current value].\n\
                 Names omit the prefix {prefix:?} shared by every field; fill_form accepts them as shown. \
                 Long sentences repeated from an earlier field are shown once.{notes}\n{rendered}"
            ))])
        })
        .await
    }

    #[tool(
        description = "Fill fields of a PDF form. values maps field names (from list_form_fields) to values. \
        Valid values are written even if others are rejected; the result lists what landed in which field and \
        every rejected name with the reason. Updates the PDF in place unless output is given."
    )]
    async fn fill_form(&self, Parameters(args): Parameters<FillFormArgs>) -> Result<CallToolResult, McpError> {
        let pdf = self.resolve(&args.pdf);
        let output = args.output.as_deref().map(|o| self.resolve_new(o));
        blocking(move || {
            let pdf = pdf?;
            let output = output.transpose()?;
            let mut values = BTreeMap::new();
            let mut problems = Vec::new();
            for (name, value) in &args.values {
                match forms::value_from_json(name, value) {
                    Ok(v) => {
                        values.insert(name.clone(), v);
                    }
                    Err(e) => problems.push(e.to_string()),
                }
            }
            let (path, filled) = forms::fill(&pdf, &values, output.as_deref())?;
            problems.extend(filled.problems.iter().cloned());
            let report = fill_report(&filled, &problems, &path);
            if filled.pdf.is_none() {
                bail!("{report}");
            }
            Ok(vec![ContentBlock::text(report)])
        })
        .await
    }

    #[tool(
        description = "Render one page of a PDF (with its filled form values) as an image and list the filled \
        values on it, so you can check it. open=true also puts the PDF on the user's screen; it is not needed to see \
        the page yourself."
    )]
    async fn view_pdf(&self, Parameters(args): Parameters<ViewPdfArgs>) -> Result<CallToolResult, McpError> {
        let pdf = self.resolve(&args.pdf);
        let data_dir = self.paths.data_dir.clone();
        blocking(move || {
            let pdf = pdf?;
            let page = args.page.unwrap_or(1);
            let rendered = forms::render(&data_dir, &pdf, page, RENDER_WIDTH)?;
            let mut note = format!("{} — page {page} of {}", pdf.display(), rendered.page_count);
            if args.open && std::env::var("NII_NO_VIEWER").is_ok_and(|v| !v.is_empty() && v != "0") {
                note.push_str(" (not opened: this server has no viewer)");
            } else if args.open {
                open::that_detached(&pdf).with_context(|| format!("opening {}", pdf.display()))?;
                note.push_str(" (opened in the default PDF viewer)");
            }
            if page < rendered.page_count {
                note.push_str(&format!("; view page={} next", page + 1));
            }
            let fields = nii_pdf::acroform::read_fields(&std::fs::read(&pdf)?)?;
            let prefix = nii_pdf::acroform::common_prefix(fields.iter().map(|f| f.name.as_str())).to_string();
            let filled: Vec<String> = fields
                .iter()
                .filter(|f| f.page == Some(page))
                .filter_map(|f| {
                    let value = f.value.as_deref()?;
                    let shown = match f.kind {
                        nii_pdf::acroform::FieldKind::Text { .. } => format!("{value:?}"),
                        _ => "checked".to_string(),
                    };
                    Some(field_line(&f.name, &prefix, f.description.as_deref(), &shown))
                })
                .collect();
            if filled.is_empty() {
                note.push_str("\nNo fields on this page are filled.");
            } else {
                note.push_str(&format!("\nFilled fields on this page:\n{}", filled.join("\n")));
            }
            Ok(vec![
                ContentBlock::text(note),
                ContentBlock::image(
                    base64::engine::general_purpose::STANDARD.encode(&rendered.png),
                    "image/png",
                ),
            ])
        })
        .await
    }

    #[tool(
        description = "List the files in the user's tax workspace (source documents, notes, and filled forms), \
        with sizes. Paths are relative to the workspace and can be passed to read_document or view_pdf."
    )]
    async fn list_documents(
        &self,
        Parameters(args): Parameters<ListDocumentsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let workspace = self.paths.workspace.clone();
        blocking(move || {
            let entries = documents::list(&workspace, Path::new(args.folder.as_deref().unwrap_or("")))?;
            if entries.is_empty() {
                return Ok(vec![ContentBlock::text("No files.")]);
            }
            let lines: Vec<String> = entries
                .iter()
                .map(|e| format!("{}  ({} KB)", e.relative.display(), e.bytes.div_ceil(1024)))
                .collect();
            Ok(vec![ContentBlock::text(format!(
                "Workspace {}:\n{}",
                workspace.display(),
                lines.join("\n")
            ))])
        })
        .await
    }

    #[tool(
        description = "Read a document from the user's tax workspace: the text of each PDF page, or a text \
        file's contents. Pages without a text layer (scans) are marked; look at those with view_pdf."
    )]
    async fn read_document(&self, Parameters(args): Parameters<ReadDocumentArgs>) -> Result<CallToolResult, McpError> {
        let paths = self.paths.clone();
        blocking(move || {
            let path = PathBuf::from(&args.path);
            let doc = documents::read(&paths.workspace, &path, |pdf| {
                nii_pdf::text::page_texts(nii_pdf::engine::pdfium(&paths.data_dir)?, pdf)
            })?;
            Ok(vec![ContentBlock::text(doc.render(&path))])
        })
        .await
    }

    #[tool(
        description = "Evaluate arithmetic exactly. Use this for every sum, difference, product, percentage, \
        and min/max a form line needs, instead of doing math yourself."
    )]
    async fn calculate(&self, Parameters(args): Parameters<CalculateArgs>) -> Result<CallToolResult, McpError> {
        blocking(move || {
            let lines: Vec<String> = args
                .expressions
                .iter()
                .map(|e| match nii_core::calc::evaluate(e) {
                    Ok(v) => format!("{e} = {}", money(v)),
                    Err(err) => format!("{e} -> error: {err}"),
                })
                .collect();
            Ok(vec![ContentBlock::text(lines.join("\n"))])
        })
        .await
    }

    #[tool(
        description = "Compute Form 1040 line 16 tax for a tax year and filing status using the IRS Tax Table, \
        Tax Computation Worksheet, or Qualified Dividends and Capital Gain Tax Worksheet, with the worksheet lines \
        shown. Also reports that year's standard deduction for the filing status. Supports tax years 2024 and 2025."
    )]
    async fn compute_tax(&self, Parameters(args): Parameters<ComputeTaxArgs>) -> Result<CallToolResult, McpError> {
        blocking(move || {
            use nii_core::tax;
            let fig = tax::figures(args.tax_year).with_context(|| {
                let years: Vec<String> = tax::YEARS.iter().map(|y| y.year.to_string()).collect();
                format!("tax year {} is not supported (supported: {})", args.tax_year, years.join(", "))
            })?;
            let status = tax::FilingStatus::parse(&args.filing_status).with_context(|| {
                format!("unknown filing status {:?}; use single, married filing jointly, married filing separately, head of household, or qualifying surviving spouse", args.filing_status)
            })?;
            let out = tax::line16(fig, status, &tax::Line16Input {
                taxable_income: args.taxable_income,
                qualified_dividends: args.qualified_dividends,
                net_capital_gain: args.net_capital_gain,
            });
            let lines: Vec<String> = out.lines.iter().map(|(label, v)| format!("  {label}: {}", money(*v))).collect();
            Ok(vec![ContentBlock::text(format!(
                "{} {}, {}:\n{}\nForm 1040 line 16 tax: {}\n\n{} standard deduction for {}: {} (Form 1040 line 12e, \
                 before any additional amount for age 65+ or blindness). Source: {}.",
                args.tax_year,
                status,
                out.method,
                lines.join("\n"),
                money(out.tax),
                args.tax_year,
                status,
                money(fig.standard_deduction(status)),
                fig.source,
            ))])
        })
        .await
    }

    #[tool(
        description = "Search US federal tax law and IRS guidance: Title 26 U.S. Code (the Internal Revenue \
        Code) plus the current IRS instructions for Form 1040, Schedule D, Form 8949, Form 2441, Schedule 8812, and \
        Publications 17, 501, 503, and 550. Hybrid keyword + semantic search; returns text with citations, links, and \
        caveats about recent amendments."
    )]
    async fn search_tax_law(&self, Parameters(args): Parameters<SearchArgs>) -> Result<CallToolResult, McpError> {
        if !self.index.is_ready() && index::is_ready(&self.paths.db_path) {
            self.index.set(IndexState::Ready);
        }
        if !self.index.is_ready() {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "The tax-law index is not ready yet: {}. Try again shortly.",
                self.index.describe()
            ))]));
        }
        let paths = self.paths.clone();
        let embedder = self.query_embedder.clone();
        blocking(move || {
            let limit = args.limit.unwrap_or(6).clamp(1, 20);
            let query_embedding = {
                let mut slot = embedder.lock().unwrap_or_else(|e| e.into_inner());
                if slot.is_none() {
                    *slot = Some(nii_embed::Embedder::new(
                        &paths.data_dir.join("models"),
                        nii_embed::Device::default(),
                    )?);
                }
                slot.as_mut().expect("initialized above").embed_query(&args.query)?
            };
            let conn = nii_search::open(&paths.db_path)?;
            let hits = nii_search::search(&conn, &args.query, &query_embedding, None, limit)?;
            if hits.is_empty() {
                return Ok(vec![ContentBlock::text("No results.")]);
            }
            let text = hits
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let caveat = h.caveat.as_deref().map(|c| format!("⚠ {c}\n")).unwrap_or_default();
                    format!(
                        "{}. {} — {}\n{}\n{caveat}{}\n{}",
                        i + 1,
                        h.citation,
                        h.heading,
                        h.section_path,
                        h.text,
                        h.url.as_deref().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            Ok(vec![ContentBlock::text(text)])
        })
        .await
    }

    /// Absolute, `~/`-relative, or workspace-relative.
    /// A PDF the tools may read: inside the workspace, `~/` expanded.
    fn resolve(&self, path: &str) -> Result<PathBuf> {
        documents::contained(&self.paths.workspace, &expand_home(path))
    }

    /// A PDF the tools may write: in an existing workspace directory, `~/` expanded.
    fn resolve_new(&self, path: &str) -> Result<PathBuf> {
        documents::contained_new(&self.paths.workspace, &expand_home(path))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for NiiServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("notintoit", env!("CARGO_PKG_VERSION")).with_title("NotIntoIt"))
            .with_instructions(INSTRUCTIONS)
    }
}

/// "12,345.67", or "12,345" for whole amounts.
fn money(v: f64) -> String {
    let cents = nii_core::calc::round_half_up(v, 2);
    let negative = cents < 0.0;
    let whole = cents.abs().trunc() as u64;
    let frac = ((cents.abs() - cents.abs().trunc()) * 100.0).round() as u64;
    let digits = whole.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let sign = if negative { "-" } else { "" };
    if frac == 0 {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{frac:02}")
    }
}

fn fill_report(filled: &nii_pdf::fill::Filled, problems: &[String], path: &Path) -> String {
    let mut out = if filled.written.is_empty() {
        "Nothing written.".to_string()
    } else {
        let lines: Vec<String> = filled
            .written
            .iter()
            .map(|w| field_line(&w.name, &filled.prefix, w.description.as_deref(), &w.value))
            .collect();
        format!(
            "Wrote {} field(s) to {}:\n{}",
            filled.written.len(),
            path.display(),
            lines.join("\n")
        )
    };
    if !problems.is_empty() {
        out.push_str(&format!("\nRejected {} (resend only these, fixed):", problems.len()));
        for problem in problems.iter().take(FILL_PROBLEMS_SHOWN) {
            out.push_str(&format!("\n  {problem}"));
        }
        if problems.len() > FILL_PROBLEMS_SHOWN {
            out.push_str(&format!(
                "\n  ...and {} more. Copy names exactly from list_form_fields; don't invent them.",
                problems.len() - FILL_PROBLEMS_SHOWN
            ));
        }
    }
    out
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

/// `  name (description) = value`, the name without `prefix` and the description clipped.
fn field_line(name: &str, prefix: &str, description: Option<&str>, value: &str) -> String {
    let name = name.strip_prefix(prefix).unwrap_or(name);
    match description {
        Some(d) => format!("  {name} ({}) = {value}", clip(d, FILL_ECHO_DESCRIPTION)),
        None => format!("  {name} = {value}"),
    }
}

/// `text` within about `max` characters: its start and end around " ... ".
fn clip(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let head: String = chars[..max / 2].iter().collect();
    let tail: String = chars[chars.len() - max / 2..].iter().collect();
    format!("{} ... {}", head.trim_end(), tail.trim_start())
}

fn render_fields(fields: &[nii_pdf::acroform::Field], prefix: &str) -> String {
    use nii_pdf::acroform::FieldKind;
    let mut seen = std::collections::HashSet::new();
    fields
        .iter()
        .map(|f| {
            let kind = match &f.kind {
                FieldKind::Text { max_len, multiline } => {
                    let mut k = String::from(if *multiline { "text, multiline" } else { "text" });
                    if let Some(m) = max_len {
                        k.push_str(&format!(", max {m}"));
                    }
                    k
                }
                FieldKind::Checkbox { on_states } if on_states.len() <= 1 => "checkbox".to_string(),
                FieldKind::Checkbox { on_states } => format!("checkbox, states {}", on_states.join("/")),
                FieldKind::Radio { states } => format!("radio, states {}", states.join("/")),
                FieldKind::Other { pdf_type } => format!("{pdf_type}, not fillable"),
            };
            let page = f.page.map(|p| format!(", p{p}")).unwrap_or_default();
            let description = f
                .description
                .as_deref()
                .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "))
                .map(|d| drop_repeated_sentences(&d, &mut seen))
                .unwrap_or_default();
            let value = f.value.as_deref().map(|v| format!(" = {v:?}")).unwrap_or_default();
            let name = f.name.strip_prefix(prefix).unwrap_or(&f.name);
            format!("{name} ({kind}{page}) {description}{value}")
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Drop sentences of `description` already shown on an earlier field in the same listing.
fn drop_repeated_sentences(description: &str, seen: &mut std::collections::HashSet<String>) -> String {
    let kept: Vec<&str> = description
        .split_inclusive(". ")
        .filter(|sentence| {
            let key = sentence.trim();
            key.len() < REPEATED_SENTENCE_MIN || seen.insert(key.to_string())
        })
        .collect();
    kept.concat().trim().to_string()
}

/// Run blocking work off the async runtime; failures become tool errors the model can read.
async fn blocking<F>(work: F) -> Result<CallToolResult, McpError>
where
    F: FnOnce() -> Result<Vec<ContentBlock>> + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(content)) => Ok(CallToolResult::success(content)),
        Ok(Err(e)) => Ok(CallToolResult::error(vec![ContentBlock::text(format!("{e:#}"))])),
        Err(e) => Err(McpError::internal_error(format!("tool task failed: {e}"), None)),
    }
}

#[derive(Clone)]
enum IndexState {
    Starting,
    Fetching { bytes: u64, total: Option<u64> },
    Downloading,
    Parsing,
    Embedding { done: usize, total: usize, since: Instant },
    WaitingForOtherProcess,
    Ready,
    Failed { error: String, retry_at: Instant },
}

struct IndexTracker {
    state: Mutex<IndexState>,
    started: AtomicBool,
}

impl Default for IndexTracker {
    fn default() -> Self {
        Self {
            state: Mutex::new(IndexState::Starting),
            started: AtomicBool::new(false),
        }
    }
}

impl IndexTracker {
    fn set(&self, state: IndexState) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
    }

    fn get(&self) -> IndexState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn is_ready(&self) -> bool {
        matches!(self.get(), IndexState::Ready)
    }

    /// Start the background thread that builds the index, retrying with
    /// exponential backoff until it succeeds. Idempotent.
    fn ensure_building(self: &Arc<Self>, paths: &Arc<Paths>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let tracker = self.clone();
        let paths = paths.clone();
        std::thread::spawn(move || {
            let mut backoff = RETRY_FIRST;
            loop {
                match tracker.run(&paths) {
                    Ok(()) => return tracker.set(IndexState::Ready),
                    Err(e) => {
                        eprintln!(
                            "nii: tax-law index build failed, retrying in {}: {e:#}",
                            minutes(backoff)
                        );
                        tracker.set(IndexState::Failed {
                            error: format!("{e:#}"),
                            retry_at: Instant::now() + backoff,
                        });
                        std::thread::sleep(backoff);
                        backoff = (backoff * 2).min(RETRY_MAX);
                    }
                }
            }
        });
    }

    fn run(&self, paths: &Paths) -> Result<()> {
        loop {
            if index::is_ready(&paths.db_path) {
                return Ok(());
            }
            let Some(lock) = index::try_lock(&paths.db_path)? else {
                self.set(IndexState::WaitingForOtherProcess);
                std::thread::sleep(OTHER_BUILD_POLL);
                continue;
            };
            if index::is_ready(&paths.db_path) {
                return Ok(());
            }
            if let Some(url) = index::prebuilt_url() {
                eprintln!("nii: fetching prebuilt tax-law index from {url}");
                match index::fetch(&url, &paths.db_path, &lock, &mut |p| {
                    if let index::Progress::Fetching { bytes, total } = p {
                        self.set(IndexState::Fetching { bytes, total });
                    }
                }) {
                    Ok(bytes) => {
                        eprintln!(
                            "nii: installed prebuilt index ({bytes} bytes) at {}",
                            paths.db_path.display()
                        );
                        return Ok(());
                    }
                    Err(e) => eprintln!("nii: no prebuilt index ({e:#}); building locally"),
                }
            }
            eprintln!("nii: building tax-law index at {}", paths.db_path.display());
            let built = index::build(
                &paths.data_dir,
                &paths.db_path,
                &lock,
                &index::BuildOptions::default(),
                &mut |p| {
                    self.set(match p {
                        index::Progress::Fetching { bytes, total } => IndexState::Fetching { bytes, total },
                        index::Progress::Downloading => IndexState::Downloading,
                        index::Progress::Parsing => IndexState::Parsing,
                        index::Progress::Embedding { done, total } => {
                            let since = match self.get() {
                                IndexState::Embedding { since, .. } => since,
                                _ => Instant::now(),
                            };
                            IndexState::Embedding { done, total, since }
                        }
                    })
                },
            )?;
            eprintln!(
                "nii: indexed {} chunks (Title 26 @ {}) in {:.0?}",
                built.chunks, built.release_point, built.elapsed
            );
            return Ok(());
        }
    }

    fn describe(&self) -> String {
        match self.get() {
            IndexState::Starting => "starting".into(),
            IndexState::Fetching { bytes, total } => format!(
                "downloading the prebuilt index ({:.1}{} MB)",
                bytes as f64 / 1e6,
                total.map(|t| format!(" of {:.1}", t as f64 / 1e6)).unwrap_or_default()
            ),
            IndexState::Downloading => "downloading Title 26 of the U.S. Code from uscode.house.gov".into(),
            IndexState::Parsing => "parsing the Internal Revenue Code into sections".into(),
            IndexState::Embedding { done, total, since } => {
                let elapsed = since.elapsed();
                let remaining = (done > 0).then(|| elapsed.mul_f64((total - done) as f64 / done as f64));
                format!(
                    "embedding {done} of {total} chunks ({}%){}",
                    done * 100 / total.max(1),
                    remaining
                        .map(|r| format!(", about {} remaining at the current rate", minutes(r)))
                        .unwrap_or_default()
                )
            }
            IndexState::WaitingForOtherProcess => "another NotIntoIt process is building it".into(),
            IndexState::Ready => "ready".into(),
            IndexState::Failed { error, retry_at } => format!(
                "the last build attempt failed ({error}); retrying in {}",
                minutes(retry_at.saturating_duration_since(Instant::now()))
            ),
        }
    }
}

fn minutes(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_formatting() {
        assert_eq!(money(76_488.0), "76,488");
        assert_eq!(money(5_578.5), "5,578.50");
        assert_eq!(money(-136.0), "-136");
        assert_eq!(money(0.0), "0");
        assert_eq!(money(1_000_000.004), "1,000,000");
    }

    #[test]
    fn clip_keeps_both_ends() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(
            clip("Row: 8. Column: Adjustment, if any. (g) Amount", 20),
            "Row: 8. Co ... (g) Amount"
        );
        assert_eq!(clip("ééééééééééé", 4), "éé ... éé");
    }

    #[test]
    fn repeated_long_sentences_are_shown_once() {
        let mut seen = std::collections::HashSet::new();
        let row = |n: u32| {
            format!(
                "Row: {n}. Column: Adjustment, if any, to gain or loss. If you enter an amount in column (g), enter a code in column (f). See the separate instructions. (g) Amount of adjustment."
            )
        };
        assert_eq!(drop_repeated_sentences(&row(1), &mut seen), row(1));
        assert_eq!(
            drop_repeated_sentences(&row(2), &mut seen),
            "Row: 2. Column: Adjustment, if any, to gain or loss. See the separate instructions. (g) Amount of adjustment."
        );
    }
}
