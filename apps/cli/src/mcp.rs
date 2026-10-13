//! `nii mcp`: stdio MCP server for IRS form fetch/inspect/fill/view and tax-law search,
//! building the search index in the background. Diagnostics go to stderr.

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::model::{PromptMessage, Role};
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt, prompt, prompt_handler, prompt_router, tool, tool_handler,
    tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{documents, forms, index, review, validate};

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

Use get_form_instructions with the filing tax_year for form-specific instructions; navigate with page or search. Unresolved instructions require manual lookup, never assume another edition applies.\n\
Forms: fetch_irs_form -> list_form_fields -> fill_form -> view_pdf. fetch_irs_form takes names like \
\"1040\", \"Schedule C\", \"8812\", \"W-2\". Use each field's description to decide which line it is, and copy its name \
as listed (e.g. Page1[0].f1_14[0]); never make names up or number them yourself. Fill only lines that have an \
entry and leave the rest blank. Checkboxes take true/false. Example values: {\"Page1[0].f1_14[0]\": \"Alex\", \
\"Page1[0].f1_47[0]\": \"50000\", \"Page1[0].c1_2[0]\": false}. fill_form can be called repeatedly on the same PDF; \
earlier values are kept. It writes every valid value, rejects bad ones individually (resend only those, fixed), \
and lists what landed in which field: check that each value is on the line you meant. view_pdf shows a page and \
its filled values. The return is done when every form it needs has been filled with fill_form; a summary of \
amounts is not a return.

Before declaring completion, call validate_return on the final PDFs. Correct every blocking error and \
run it again after edits. Supply the W-2 source paths and set w2_complete only after the user/documents \
establish that every W-2 is present. checks_passed covers only the listed checks, not filing readiness. \
Report remaining review items explicitly; never claim unresolved or skipped checks passed.
Before disputing a finding or recommending undoing a repair, read the relevant year's IRS instructions with \
get_form_instructions and cite the delivered page and exact excerpt. Unsupported opinions do not override \
validation. After all edits, call create_review_report with the same W-2 sources. Use its status and \
allowed_final_statement in the final response and link its report paths. blocked and needs_review reports \
are incomplete; report every unresolved finding/dispute. Do not claim all checks passed based only on a \
correct refund. Any later PDF or W-2 edit requires a new report.

Law: search_tax_law covers Title 26 and the current IRS instructions and publications. Pass tax_year for the \
return being prepared to flag mismatched guidance editions. Cite what you rely on. \
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
    instruction_reads: Arc<Mutex<Vec<crate::instructions::ReadEvidence>>>,
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}

#[derive(Deserialize, JsonSchema)]
pub struct InstructionsArgs {
    /// Form name, e.g. "1040", "Schedule C", "Schedule SE", "8812".
    form: String,
    /// Required tax year; never silently substitutes another year's instructions.
    tax_year: u16,
    /// Read this 1-based page. Defaults to page 1 unless searching.
    page: Option<u32>,
    /// Case-insensitive text search; returns up to three matching pages.
    search: Option<String>,
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
    /// Return's tax year. Flags guidance from other or unknown editions; does not retrieve historical law.
    tax_year: Option<u16>,
    /// Maximum results (1-20). Default 6.
    limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ValidateReturnArgs {
    /// Tax year. The current deterministic rule set supports 2025.
    tax_year: u16,
    /// Workspace-relative directory of proposed filled forms. Default: <tax_year>/forms.
    forms_dir: Option<String>,
    /// Paths to all applicable W-2 source documents. Supports fillable IRS PDFs and labelled text transcriptions.
    #[serde(default)]
    w2_documents: Vec<String>,
    /// True only when the user/intake confirms these are all W-2s for the return. Missing/unreadable sources never imply zero wages.
    #[serde(default)]
    w2_complete: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReviewArgs {
    #[serde(flatten)]
    validation: ValidateReturnArgs,
    /// Disagreements with current findings. Cite a page and exact quote returned by
    /// get_form_instructions in this MCP session. Never waives a finding.
    #[serde(default)]
    disputes: Vec<review::Dispute>,
}

#[tool_router]
impl NiiServer {
    fn new(paths: Paths) -> Self {
        Self {
            paths: Arc::new(paths),
            index: Arc::new(IndexTracker::default()),
            query_embedder: Arc::new(Mutex::new(None)),
            instruction_reads: Arc::new(Mutex::new(Vec::new())),
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
        }
    }

    #[tool(
        description = "Get year-pinned IRS form instructions with source URL, local PDF and readable text. Resolves shared and embedded instructions for known forms, otherwise tries the conventional instruction filename. Defaults to the first page; use page or search to navigate. Reports unavailable editions without substituting older/current instructions."
    )]
    async fn get_form_instructions(
        &self,
        Parameters(args): Parameters<InstructionsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let paths = self.paths.clone();
        let reads = self.instruction_reads.clone();
        blocking(move || {
            let retrieved = crate::instructions::read_with_evidence(
                &paths.data_dir,
                &paths.workspace,
                &args.form,
                args.tax_year,
                args.page,
                args.search.as_deref(),
            )?;
            let receipt = serde_json::to_string(&retrieved.evidence)?;
            reads.lock().unwrap_or_else(|e| e.into_inner()).push(retrieved.evidence);
            Ok(vec![ContentBlock::text(format!(
                "{}\n\nSource access recorded for final review: {receipt}",
                retrieved.text
            ))])
        })
        .await
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
            let shown = matching_fields(fields, args.page, args.search.as_deref());
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
            if values.is_empty() && !problems.is_empty() {
                bail!("Nothing written.{}", rejected(&problems));
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
        description = "Read-only deterministic checks on filled 2025 IRS forms: identities, filing status, required nonzero lines, integer-cent arithmetic, schedule transfers, and owner-specific Schedule SE wages against supplied W-2 sources. Returns blocked/needs_review/checks_passed with exact field names for fixes and explicit coverage limits. Does not use benchmark answers, edit forms, certify eligibility, or prove filing readiness. Run after filling and again after corrections."
    )]
    async fn validate_return(
        &self,
        Parameters(args): Parameters<ValidateReturnArgs>,
    ) -> Result<CallToolResult, McpError> {
        let paths = self.paths.clone();
        let result = tokio::task::spawn_blocking(move || {
            validate::run(
                &paths.workspace,
                args.tax_year,
                args.forms_dir.as_deref(),
                &args.w2_documents,
                args.w2_complete,
            )
        })
        .await;
        match result {
            Ok(Ok(report)) => {
                let content = vec![ContentBlock::text(
                    serde_json::to_string_pretty(&report).map_err(|e| McpError::internal_error(e.to_string(), None))?,
                )];
                let mut result = if report.blocking_errors > 0 {
                    CallToolResult::error(content)
                } else {
                    CallToolResult::success(content)
                };
                result.structured_content =
                    Some(serde_json::to_value(report).map_err(|e| McpError::internal_error(e.to_string(), None))?);
                Ok(result)
            }
            Ok(Err(e)) => Ok(CallToolResult::error(vec![ContentBlock::text(format!("{e:#}"))])),
            Err(e) => Err(McpError::internal_error(format!("validation task failed: {e}"), None)),
        }
    }

    #[tool(
        description = "Create and save a final return review report from the current PDFs and W-2 sources. Reruns validation, captures file hashes, lists unresolved findings and IRS source links, and records instruction pages actually returned in this MCP session. Optional sourced disputes remain pending manual review and never override findings. Returns blocked/needs_review/checks_passed plus the allowed final statement and JSON/Markdown paths. Run after all edits; later edits invalidate the report. Does not certify filing readiness."
    )]
    async fn create_review_report(&self, Parameters(args): Parameters<ReviewArgs>) -> Result<CallToolResult, McpError> {
        let paths = self.paths.clone();
        let reads = self.instruction_reads.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let result = tokio::task::spawn_blocking(move || {
            let a = args.validation;
            review::create(
                &paths.workspace,
                a.tax_year,
                a.forms_dir.as_deref(),
                &a.w2_documents,
                a.w2_complete,
                &reads,
                &args.disputes,
            )
        })
        .await;
        match result {
            Ok(Ok(report)) => {
                let content = vec![ContentBlock::text(
                    serde_json::to_string_pretty(&report).map_err(|e| McpError::internal_error(e.to_string(), None))?,
                )];
                let mut result = if report.status != "checks_passed" {
                    CallToolResult::error(content)
                } else {
                    CallToolResult::success(content)
                };
                result.structured_content =
                    Some(serde_json::to_value(report).map_err(|e| McpError::internal_error(e.to_string(), None))?);
                Ok(result)
            }
            Ok(Err(e)) => Ok(CallToolResult::error(vec![ContentBlock::text(format!("{e:#}"))])),
            Err(e) => Err(McpError::internal_error(format!("review task failed: {e}"), None)),
        }
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
        blocking(move || Ok(vec![ContentBlock::text(tax_report(&args)?)])).await
    }

    #[tool(
        description = "Search US federal tax law and IRS guidance: Title 26 U.S. Code (the Internal Revenue \
        Code) plus the current IRS instructions for Form 1040, Schedule D, Form 8949, Form 2441, Schedule 8812, Schedules C/E/SE, Form 8995, and \
        Publications 17, 501, 503, 550, 334, 527, and 925. Hybrid keyword + semantic search; returns text with citations, links, and \
        edition and source-download provenance, and caveats about recent amendments."
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
                    let year_warning = if matches!(
                        h.source,
                        nii_core::SourceKind::IrsPub | nii_core::SourceKind::IrsInstruction
                    ) {
                        guidance_year_warning(&h.citation, args.tax_year)
                    } else {
                        String::new()
                    };
                    format!(
                        "{}. {} — {}\n{}\n{year_warning}{caveat}{}\n{}",
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

#[derive(Deserialize, JsonSchema)]
pub struct TaxWizardArgs {
    /// Optional four-digit tax year, e.g. "2025". If omitted, the interview asks for it.
    tax_year: Option<String>,
}

#[prompt_router]
impl NiiServer {
    #[prompt(
        name = "tax-wizard",
        description = "Start or resume a guided personal/family federal tax interview, including side-business and rental follow-ups. Optional tax_year."
    )]
    async fn tax_wizard(&self, Parameters(args): Parameters<TaxWizardArgs>) -> Result<Vec<PromptMessage>, McpError> {
        let year = match args.tax_year {
            Some(raw) => {
                let raw = raw.trim();
                if raw.len() != 4
                    || !raw.bytes().all(|b| b.is_ascii_digit())
                    || !raw.parse::<u16>().is_ok_and(|year| (1900..=2099).contains(&year))
                {
                    return Err(McpError::invalid_params(
                        "tax_year must be a four-digit year between 1900 and 2099",
                        None,
                    ));
                }
                format!("Tax year requested: {raw}. Use workspace documents under {raw}/.\n\n")
            }
            None => "Tax year not specified. Ask me which year before selecting documents or tax rules.\n\n".into(),
        };
        Ok(vec![PromptMessage::new_text(
            Role::User,
            format!("{year}{}", include_str!("../prompts/tax-wizard.md")),
        )])
    }
}

fn guidance_year_warning(citation: &str, requested: Option<u16>) -> String {
    let Some(requested) = requested else {
        return String::new();
    };
    let edition = citation
        .rsplit('(')
        .filter_map(|tail| tail.split_once(')'))
        .find_map(|(year, _)| year.parse::<u16>().ok());
    match edition {
        Some(year) if year == requested => String::new(),
        Some(year) => format!(
            "⚠ Tax-year mismatch: requested {requested}, guidance edition {year}. Verify the {requested} instructions before applying these rules.\n"
        ),
        None => format!("⚠ Cannot verify guidance edition for requested tax year {requested}.\n"),
    }
}

#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for NiiServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_prompts().build())
            .with_server_info(Implementation::new("notintoit", env!("CARGO_PKG_VERSION")).with_title("NotIntoIt"))
            .with_instructions(INSTRUCTIONS)
    }
}

/// Line 16 tax for `args`, with the worksheet lines and the year's standard deduction.
fn tax_report(args: &ComputeTaxArgs) -> Result<String> {
    use nii_core::tax;
    let fig = tax::figures(args.tax_year).with_context(|| {
        let years: Vec<String> = tax::YEARS.iter().map(|y| y.year.to_string()).collect();
        format!(
            "tax year {} is not supported (supported: {})",
            args.tax_year,
            years.join(", ")
        )
    })?;
    let status = tax::FilingStatus::parse(&args.filing_status).with_context(|| {
        format!("unknown filing status {:?}; use single, married filing jointly, married filing separately, head of household, or qualifying surviving spouse", args.filing_status)
    })?;
    let out = tax::line16(
        fig,
        status,
        &tax::Line16Input {
            taxable_income: args.taxable_income,
            qualified_dividends: args.qualified_dividends,
            net_capital_gain: args.net_capital_gain,
        },
    );
    let lines: Vec<String> = out
        .lines
        .iter()
        .map(|(label, v)| format!("  {label}: {}", money(*v)))
        .collect();
    Ok(format!(
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
    ))
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

/// Fields on `page` whose name or description contains `search`, ignoring case.
fn matching_fields(
    fields: Vec<nii_pdf::acroform::Field>,
    page: Option<u32>,
    search: Option<&str>,
) -> Vec<nii_pdf::acroform::Field> {
    let needle = search.map(str::to_lowercase);
    fields
        .into_iter()
        .filter(|f| page.is_none_or(|p| f.page == Some(p)))
        .filter(|f| {
            needle.as_deref().is_none_or(|n| {
                f.name.to_lowercase().contains(n)
                    || f.description.as_deref().is_some_and(|d| d.to_lowercase().contains(n))
            })
        })
        .collect()
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
    out.push_str(&rejected(problems));
    out
}

/// The rejected values as lines to resend, or nothing when there are none.
fn rejected(problems: &[String]) -> String {
    if problems.is_empty() {
        return String::new();
    }
    let mut out = format!("\nRejected {} (resend only these, fixed):", problems.len());
    for problem in problems.iter().take(FILL_PROBLEMS_SHOWN) {
        out.push_str(&format!("\n  {problem}"));
    }
    if problems.len() > FILL_PROBLEMS_SHOWN {
        out.push_str(&format!(
            "\n  ...and {} more. Copy names exactly from list_form_fields; don't invent them.",
            problems.len() - FILL_PROBLEMS_SHOWN
        ));
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
    use crate::testutil;

    fn server(label: &str) -> NiiServer {
        let workspace = testutil::scratch(label);
        NiiServer::new(Paths {
            data_dir: testutil::data_dir(),
            db_path: workspace.join("index.db"),
            workspace,
        })
    }

    /// (is_error, text blocks joined, image count) of a tool result.
    fn unpack(result: Result<CallToolResult, McpError>) -> (bool, String, usize) {
        let value = serde_json::to_value(result.expect("tool returned a protocol error")).unwrap();
        let blocks = value["content"].as_array().cloned().unwrap_or_default();
        let text = blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let images = blocks.iter().filter(|b| b["type"] == "image").count();
        (value["isError"] == true, text, images)
    }

    async fn fetched(label: &str) -> NiiServer {
        let s = server(label);
        let (err, text, _) = unpack(
            s.fetch_irs_form(Parameters(FetchFormArgs {
                form: "1040".into(),
                tax_year: 2025,
                overwrite: false,
            }))
            .await,
        );
        assert!(!err, "{text}");
        s
    }

    #[tokio::test]
    async fn fetch_irs_form_creates_then_keeps_the_working_copy() {
        let s = server("fetch");
        let args = || {
            Parameters(FetchFormArgs {
                form: "1040".into(),
                tax_year: 2025,
                overwrite: false,
            })
        };
        let (err, text, _) = unpack(s.fetch_irs_form(args()).await);
        assert!(!err);
        assert!(text.starts_with("Created from the cached download -> "), "{text}");
        assert!(
            text.contains("2025/forms/f1040.pdf\n199 fillable fields. Next: list_form_fields"),
            "{text}"
        );

        let (_, text, _) = unpack(s.fetch_irs_form(args()).await);
        assert!(
            text.starts_with("Kept existing working copy (0 fields already filled; pass overwrite=true"),
            "{text}"
        );

        let (err, text, _) = unpack(
            s.fetch_irs_form(Parameters(FetchFormArgs {
                form: " - ".into(),
                tax_year: 2025,
                overwrite: false,
            }))
            .await,
        );
        assert!(err && text.contains("empty form name"), "{text}");
    }

    #[tokio::test]
    async fn list_form_fields_filters_strips_the_prefix_and_shows_values() {
        let s = fetched("list").await;
        let list = |page, search: Option<&str>| {
            s.list_form_fields(Parameters(ListFieldsArgs {
                pdf: "2025/forms/f1040.pdf".into(),
                page,
                search: search.map(String::from),
            }))
        };
        let (err, text, _) = unpack(list(None, Some("social security number")).await);
        assert!(!err);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "2 of 199 fields, one per line: name (type, page, limits) description [= current value]."
        );
        assert!(lines[1].contains("prefix \"topmostSubform[0].\""), "{text}");
        assert_eq!(
            lines[2],
            "Page1[0].f1_16[0] (text, max 9, p1) Your social security number."
        );
        assert_eq!(
            lines[3],
            "Page1[0].f1_19[0] (text, max 9, p1) Spouse's social security number."
        );

        let (_, text, _) = unpack(list(Some(2), None).await);
        assert!(text.starts_with("71 of 199 fields"), "{text}");
        assert!(text.contains("(checkbox, p2)"), "{text}");

        s.fill_form(Parameters(FillFormArgs {
            pdf: "2025/forms/f1040.pdf".into(),
            values: BTreeMap::from([("f1_14[0]".into(), serde_json::json!("Jane"))]),
            output: None,
        }))
        .await
        .unwrap();
        let (_, text, _) = unpack(list(None, Some("first name and middle")).await);
        assert!(
            text.contains("Your first name and middle initial. = \"Jane\""),
            "{text}"
        );

        let (err, text, _) = unpack(
            s.list_form_fields(Parameters(ListFieldsArgs {
                pdf: "missing.pdf".into(),
                page: None,
                search: None,
            }))
            .await,
        );
        assert!(err && text.starts_with("missing.pdf does not exist"), "{text}");
    }

    #[tokio::test]
    async fn fill_form_reports_counts_and_rejects_bad_values() {
        let s = fetched("fill").await;
        let fill = |values: serde_json::Value, output: Option<&str>| {
            s.fill_form(Parameters(FillFormArgs {
                pdf: "2025/forms/f1040.pdf".into(),
                values: serde_json::from_value(values).unwrap(),
                output: output.map(String::from),
            }))
        };
        let (err, text, _) = unpack(fill(serde_json::json!({"f1_14[0]": "Jane", "f1_47[0]": 72805}), None).await);
        assert!(!err);
        assert!(text.starts_with("Wrote 2 field(s) to "), "{text}");
        assert!(
            text.contains("\n  Page1[0].f1_14[0] (Your first name and middle initial.) = \"Jane\""),
            "{text}"
        );
        assert!(text.ends_with(" = \"72805\""), "{text}");

        let (err, text, _) = unpack(fill(serde_json::json!({"f1_14[0]": "Jane", "f9_99[0]": "x"}), None).await);
        assert!(!err);
        assert!(text.starts_with("Wrote 1 field(s) to "), "{text}");
        assert!(
            text.ends_with("\nRejected 1 (resend only these, fixed):\n  f9_99[0]: no such field"),
            "{text}"
        );

        let (_, text, _) = unpack(fill(serde_json::json!({"f1_14[0]": "Jo"}), Some("2025/forms/copy.pdf")).await);
        assert!(text.contains("2025/forms/copy.pdf"), "{text}");

        let (err, text, _) = unpack(fill(serde_json::json!({"f1_14[0]": ["x"]}), None).await);
        assert!(err);
        assert_eq!(
            text,
            "Nothing written.\nRejected 1 (resend only these, fixed):\n  f1_14[0]: expected a string, number, or boolean, got [\"x\"]"
        );

        let (err, text, _) = unpack(fill(serde_json::json!({"f1_14[0]": "x"}), Some("out.txt")).await);
        assert!(err && text.starts_with("output must be a .pdf path"), "{text}");
    }

    #[tokio::test]
    async fn view_pdf_returns_a_note_and_an_image() {
        let s = fetched("view").await;
        let view = |page| {
            s.view_pdf(Parameters(ViewPdfArgs {
                pdf: "2025/forms/f1040.pdf".into(),
                page,
                open: false,
            }))
        };
        let (err, text, images) = unpack(view(Some(2)).await);
        assert!(!err);
        assert!(
            text.contains("2025/forms/f1040.pdf — page 2 of 2\nNo fields on this page are filled."),
            "{text}"
        );
        assert_eq!(images, 1);

        let (err, text, images) = unpack(view(Some(3)).await);
        assert!(err && images == 0);
        assert_eq!(text, "page 3 out of range (document has 2)");
    }

    #[tokio::test]
    async fn documents_are_listed_and_read_from_the_workspace_only() {
        let s = server("docs");
        let (_, text, _) = unpack(s.list_documents(Parameters(ListDocumentsArgs { folder: None })).await);
        assert_eq!(text, "No files.");

        std::fs::create_dir_all(s.paths.workspace.join("2025")).unwrap();
        std::fs::write(s.paths.workspace.join("2025/notes.md"), "x".repeat(1500)).unwrap();
        let (_, text, _) = unpack(
            s.list_documents(Parameters(ListDocumentsArgs {
                folder: Some("2025".into()),
            }))
            .await,
        );
        assert!(
            text.starts_with("Workspace ") && text.ends_with("\n2025/notes.md  (2 KB)"),
            "{text}"
        );

        let read = |path: &str| s.read_document(Parameters(ReadDocumentArgs { path: path.into() }));
        let (err, text, _) = unpack(read("2025/notes.md").await);
        assert!(!err && text == "x".repeat(1500));
        let (err, text, _) = unpack(read("/etc/hostname").await);
        assert!(err && text.contains("outside the tax workspace"), "{text}");
    }

    #[tokio::test]
    async fn calculate_evaluates_each_expression() {
        let s = server("calc");
        let (err, text, _) = unpack(
            s.calculate(Parameters(CalculateArgs {
                expressions: vec!["72,805 + 665".into(), "1 / 0".into(), "0.5 * 3".into()],
            }))
            .await,
        );
        assert!(!err);
        assert_eq!(
            text,
            "72,805 + 665 = 73,470\n1 / 0 -> error: division by zero\n0.5 * 3 = 1.50"
        );
    }

    #[tokio::test]
    async fn compute_tax_shows_the_worksheet_and_rejects_unknown_inputs() {
        let s = server("tax");
        let args = |year, status: &str| {
            Parameters(ComputeTaxArgs {
                tax_year: year,
                filing_status: status.into(),
                taxable_income: 51_678.0,
                qualified_dividends: 413.0,
                net_capital_gain: 2_532.0,
            })
        };
        let (err, text, _) = unpack(s.compute_tax(args(2025, "head of household")).await);
        assert!(!err);
        assert!(text.starts_with("2025 Head of household, Qualified Dividends and Capital Gain Tax Worksheet:\n  1 Taxable income: 51,678"), "{text}");
        assert!(text.contains("\nForm 1040 line 16 tax: 5,507\n"), "{text}");
        assert!(
            text.contains("2025 standard deduction for Head of household: 23,625"),
            "{text}"
        );

        let (err, text, _) = unpack(s.compute_tax(args(2023, "single")).await);
        assert!(err);
        assert_eq!(text, "tax year 2023 is not supported (supported: 2024, 2025)");
        let (err, text, _) = unpack(s.compute_tax(args(2025, "widow")).await);
        assert!(err && text.starts_with("unknown filing status \"widow\""), "{text}");
    }

    #[tokio::test]
    async fn search_reports_progress_until_the_index_is_ready() {
        let s = server("search");
        s.index.set(IndexState::Parsing);
        let (err, text, _) = unpack(
            s.search_tax_law(Parameters(SearchArgs {
                query: "x".into(),
                tax_year: Some(2025),
                limit: None,
            }))
            .await,
        );
        assert!(err);
        assert_eq!(
            text,
            "The tax-law index is not ready yet: parsing the Internal Revenue Code into sections. Try again shortly."
        );
    }

    #[tokio::test]
    async fn final_review_is_discoverable_and_keeps_blocked_and_review_statuses_as_tool_errors() {
        let s = fetched("final-review").await;
        let schema = serde_json::to_value(s.tool_router.list_all()).unwrap();
        let tool = schema
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "create_review_report")
            .unwrap();
        assert!(tool["inputSchema"]["properties"]["tax_year"].is_object());
        assert!(tool["inputSchema"]["properties"]["disputes"].is_object());
        let result = s
            .create_review_report(Parameters(ReviewArgs {
                validation: ValidateReturnArgs {
                    tax_year: 2025,
                    forms_dir: None,
                    w2_documents: vec![],
                    w2_complete: false,
                },
                disputes: vec![],
            }))
            .await
            .unwrap();
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["isError"], true);
        assert_eq!(value["structuredContent"]["status"], "blocked");
        assert!(s.paths.workspace.join("2025/review/return-review.json").exists());
        let result = s
            .create_review_report(Parameters(ReviewArgs {
                validation: ValidateReturnArgs {
                    tax_year: 2024,
                    forms_dir: Some("2025/forms".into()),
                    w2_documents: vec![],
                    w2_complete: false,
                },
                disputes: vec![],
            }))
            .await
            .unwrap();
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["isError"], true);
        assert_eq!(value["structuredContent"]["status"], "needs_review");
    }

    #[test]
    fn paths_resolve_inside_the_workspace_only() {
        let s = server("resolve");
        let root = s.paths.workspace.canonicalize().unwrap();
        std::fs::create_dir_all(root.join("2025/forms")).unwrap();
        std::fs::write(root.join("2025/forms/f1040.pdf"), "").unwrap();
        let form = root.join("2025/forms/f1040.pdf");
        assert_eq!(s.resolve("2025/forms/f1040.pdf").unwrap(), form);
        assert_eq!(s.resolve(form.to_str().unwrap()).unwrap(), form);
        assert_eq!(
            s.resolve_new("2025/forms/f1040-copy.pdf").unwrap(),
            root.join("2025/forms/f1040-copy.pdf")
        );
        let outside = s.resolve("/etc/hostname").unwrap_err().to_string();
        assert!(outside.contains("outside the tax workspace"), "{outside}");
        assert!(s.resolve("../escape.pdf").is_err());
        assert!(s.resolve_new("/tmp/escape.pdf").is_err());
        assert!(s.resolve("~/x.pdf").is_err());
    }

    #[test]
    fn server_info_names_the_server_and_carries_instructions() {
        let info = server("info").get_info();
        assert_eq!(info.server_info.name, "notintoit");
        assert!(
            info.instructions
                .unwrap()
                .starts_with("NotIntoIt prepares US federal returns")
        );
    }

    #[test]
    fn wizard_is_discoverable_with_an_optional_string_year() {
        let s = server("wizard-list");
        let prompts = serde_json::to_value(s.prompt_router.list_all()).unwrap();
        assert_eq!(prompts.as_array().unwrap().len(), 1);
        assert_eq!(prompts[0]["name"], "tax-wizard");
        assert_eq!(prompts[0]["arguments"][0]["name"], "tax_year");
        assert_ne!(prompts[0]["arguments"][0]["required"], true);
        let capabilities = serde_json::to_value(s.get_info()).unwrap();
        assert!(capabilities["capabilities"]["prompts"].is_object());
        assert!(capabilities["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn validation_tool_reports_blocking_findings_as_structured_tool_errors() {
        let s = fetched("validate-tool").await;
        let result = s
            .validate_return(Parameters(ValidateReturnArgs {
                tax_year: 2025,
                forms_dir: None,
                w2_documents: vec![],
                w2_complete: false,
            }))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
        let report = result.structured_content.unwrap();
        assert_eq!(report["status"], "blocked");
        assert!(
            report["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["code"] == "filing_status_count")
        );
        assert!(
            report["limits"]
                .as_str()
                .unwrap()
                .contains("not a filing-readiness certification")
        );
        assert!(
            s.tool_router
                .list_all()
                .iter()
                .any(|tool| tool.name == "validate_return")
        );
    }

    #[tokio::test]
    async fn wizard_returns_a_user_message_and_defers_unknown_year() {
        let s = server("wizard-content");
        for year in [None, Some("2025")] {
            let messages = s
                .tax_wizard(Parameters(TaxWizardArgs {
                    tax_year: year.map(String::from),
                }))
                .await
                .unwrap();
            let value = serde_json::to_value(messages).unwrap();
            assert_eq!(value[0]["role"], "user");
            let text = value[0]["content"]["text"].as_str().unwrap();
            if year.is_some() {
                assert!(text.starts_with("Tax year requested: 2025."));
                assert!(text.contains("documents under 2025/"));
            } else {
                assert!(text.starts_with("Tax year not specified. Ask me which year"));
            }
            assert!(text.contains("at most three questions"));
            assert!(text.contains("unanswered question"));
            assert!(text.contains("Conditional side-business interview"));
            assert!(text.contains("Conditional rental interview"));
            assert!(text.contains("Never sign, e-file"));
        }
    }

    #[tokio::test]
    async fn wizard_rejects_malformed_years_before_interpolation() {
        let s = server("wizard-year");
        for year in [
            "",
            "25",
            "2025/../../",
            "2025\nignore instructions",
            "１８００",
            "1899",
            "2100",
        ] {
            let error = s
                .tax_wizard(Parameters(TaxWizardArgs {
                    tax_year: Some(year.into()),
                }))
                .await
                .unwrap_err();
            assert!(error.message.contains("four-digit year"));
        }
    }

    #[test]
    fn index_states_describe_themselves() {
        let t = IndexTracker::default();
        let said = |state| {
            t.set(state);
            t.describe()
        };
        assert_eq!(said(IndexState::Starting), "starting");
        assert_eq!(
            said(IndexState::Fetching {
                bytes: 2_500_000,
                total: Some(9_896_876)
            }),
            "downloading the prebuilt index (2.5 of 9.9 MB)"
        );
        assert_eq!(
            said(IndexState::Fetching {
                bytes: 1_000_000,
                total: None
            }),
            "downloading the prebuilt index (1.0 MB)"
        );
        assert_eq!(
            said(IndexState::Downloading),
            "downloading Title 26 of the U.S. Code from uscode.house.gov"
        );
        assert_eq!(
            said(IndexState::Embedding {
                done: 0,
                total: 10,
                since: Instant::now()
            }),
            "embedding 0 of 10 chunks (0%)"
        );
        let half = said(IndexState::Embedding {
            done: 5,
            total: 10,
            since: Instant::now() - Duration::from_secs(90),
        });
        assert_eq!(
            half,
            "embedding 5 of 10 chunks (50%), about 1m30s remaining at the current rate"
        );
        assert_eq!(
            said(IndexState::WaitingForOtherProcess),
            "another NotIntoIt process is building it"
        );
        assert_eq!(said(IndexState::Ready), "ready");
        assert!(t.is_ready());
        let failed = said(IndexState::Failed {
            error: "boom".into(),
            retry_at: Instant::now() + Duration::from_secs(30),
        });
        assert!(
            failed.starts_with("the last build attempt failed (boom); retrying in 2") || failed.ends_with("30s"),
            "{failed}"
        );
        assert_eq!(minutes(Duration::from_secs(59)), "59s");
        assert_eq!(minutes(Duration::from_secs(61)), "1m01s");
    }

    #[test]
    fn run_returns_at_once_when_the_index_is_ready() {
        let dir = testutil::scratch("run");
        testutil::tiny_index(&dir.join("index.db"));
        let paths = Paths {
            data_dir: dir.clone(),
            db_path: dir.join("index.db"),
            workspace: dir,
        };
        IndexTracker::default().run(&paths).unwrap();
    }

    #[test]
    fn rendered_fields_show_kind_limits_and_values() {
        use nii_pdf::acroform::{Field, FieldKind};
        let field = |name: &str, kind, value: Option<&str>| Field {
            name: name.into(),
            kind,
            description: Some("Desc.".into()),
            page: Some(1),
            value: value.map(String::from),
        };
        let fields = vec![
            field(
                "top.a",
                FieldKind::Text {
                    max_len: Some(9),
                    multiline: true,
                },
                Some("v"),
            ),
            field(
                "top.b",
                FieldKind::Checkbox {
                    on_states: vec!["1".into(), "2".into()],
                },
                None,
            ),
            field(
                "top.c",
                FieldKind::Radio {
                    states: vec!["A".into(), "B".into()],
                },
                None,
            ),
            field("top.d", FieldKind::Other { pdf_type: "Sig".into() }, None),
        ];
        assert_eq!(
            render_fields(&fields, "top."),
            "a (text, multiline, max 9, p1) Desc. = \"v\"\nb (checkbox, states 1/2, p1) Desc.\nc (radio, states A/B, p1) Desc.\nd (Sig, not fillable, p1) Desc."
        );
    }

    #[test]
    fn money_formatting() {
        assert_eq!(money(76_488.0), "76,488");
        assert_eq!(money(5_578.5), "5,578.50");
        assert_eq!(money(-136.0), "-136");
        assert_eq!(money(0.0), "0");
        assert_eq!(money(1_000_000.004), "1,000,000");
    }

    #[test]
    fn search_flags_mismatched_and_unknown_guidance_editions() {
        assert!(guidance_year_warning("Instructions for Form 1040 (2025)", Some(2025)).is_empty());
        assert!(guidance_year_warning("Publication 334 (2025), Tax Guide for Small Business", Some(2025)).is_empty());
        assert!(
            guidance_year_warning("Publication 527 (2026)", Some(2025))
                .contains("requested 2025, guidance edition 2026")
        );
        assert!(guidance_year_warning("Publication 527", Some(2025)).contains("Cannot verify"));
        assert!(guidance_year_warning("Publication 527", None).is_empty());
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
