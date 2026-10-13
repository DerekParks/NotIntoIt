//! Read-only, bounded checks on IRS forms. No answer keys and no LLM calls.
//! Money is integer cents; rules allow $1 for independently rounded form lines.
use anyhow::{Context, Result};
use nii_pdf::acroform::{Field, FieldKind};
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const TOLERANCE: i128 = 100;
const KINDS: &[&str] = &[
    "f1040sse", "f1040sc", "f1040se", "f1040s1", "f1040s2", "f1040s3", "f1040s8", "f1040sb", "f8995", "f1040",
];

#[derive(Debug, Serialize)]
pub struct Finding {
    pub severity: &'static str,
    pub code: &'static str,
    pub form: String,
    pub field: Option<String>,
    pub line: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub tax_year: u16,
    pub status: &'static str,
    pub blocking_errors: usize,
    pub review_items: usize,
    pub checks_run: usize,
    pub forms_checked: Vec<String>,
    pub w2_sources_checked: Vec<String>,
    pub w2_inventory_complete: bool,
    pub tolerance_dollars: u8,
    pub findings: Vec<Finding>,
    pub limits: &'static str,
}

impl Report {
    fn new(year: u16, complete: bool) -> Self {
        Self {
            tax_year: year,
            status: "checks_passed",
            blocking_errors: 0,
            review_items: 0,
            checks_run: 0,
            forms_checked: vec![],
            w2_sources_checked: vec![],
            w2_inventory_complete: complete,
            tolerance_dollars: 1,
            findings: vec![],
            limits: "Checks apply to 2025 calendar-year returns, supported field layouts, and listed arithmetic/transfer rules only. Passing is not a filing-readiness certification: eligibility, all source documents, elections, carryovers, tax-rate worksheets, and unimplemented lines still require review. No forms are edited.",
        }
    }
    fn finish(&mut self) {
        self.blocking_errors = self.findings.iter().filter(|f| f.severity == "error").count();
        self.review_items = self.findings.iter().filter(|f| f.severity == "review").count();
        self.status = if self.blocking_errors > 0 {
            "blocked"
        } else if self.review_items > 0 {
            "needs_review"
        } else {
            "checks_passed"
        };
    }
    fn add(
        &mut self,
        severity: &'static str,
        code: &'static str,
        form: &str,
        field: Option<&Field>,
        line: &str,
        message: String,
    ) {
        let field_name = field.map(|f| f.name.clone());
        if self.findings.iter().any(|f| {
            f.code == code && f.form == form && f.field == field_name && f.line == line && f.message == message
        }) {
            return;
        }
        self.findings.push(Finding {
            severity,
            code,
            form: form.into(),
            field: field_name,
            line: line.into(),
            message,
        });
    }
}

struct Form {
    file: String,
    kind: String,
    fields: Vec<Field>,
}
#[derive(Clone, Copy)]
struct Money {
    cents: i128,
    present: bool,
}
struct W2 {
    owner: String,
    wages: i128,
    withheld: i128,
    social: i128,
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn label_matches(description: &str, selector: &str) -> bool {
    description.match_indices(selector).any(|(offset, _)| {
        !selector.starts_with(|c: char| c.is_ascii_digit())
            || offset == 0
            || !description.as_bytes()[offset - 1].is_ascii_digit()
    })
}
fn present(f: &Field) -> bool {
    f.value.as_deref().is_some_and(|v| !v.trim().is_empty() && v != "Off")
}
fn ticked(f: &Field) -> bool {
    matches!(f.kind, FieldKind::Checkbox { .. } | FieldKind::Radio { .. }) && present(f)
}
fn tin(value: &str) -> Option<String> {
    let s: String = value.chars().filter(|c| *c != '-' && !c.is_whitespace()).collect();
    (s.len() == 9 && s.bytes().all(|c| c.is_ascii_digit())).then_some(s)
}
fn money(value: &str) -> Option<i128> {
    let s = value.trim();
    if s == "-" || s == "-0-" {
        return Some(0);
    }
    let s: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ',' && *c != '$')
        .collect();
    let (negative, s) = if s.starts_with('(') && s.ends_with(')') {
        (true, &s[1..s.len() - 1])
    } else if let Some(s) = s.strip_prefix('-') {
        (true, s)
    } else {
        (false, s.as_str())
    };
    let mut parts = s.split('.');
    let whole = parts.next()?;
    if whole.is_empty() || !whole.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let fraction = parts.next().unwrap_or("");
    if parts.next().is_some() || fraction.len() > 2 || !fraction.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let whole: i128 = whole.parse().ok()?;
    let fraction: i128 = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<i128>().ok()? * if fraction.len() == 1 { 10 } else { 1 }
    };
    let cents = whole.checked_mul(100)?.checked_add(fraction)?;
    if cents > 100_000_000_000_000 {
        return None;
    }
    Some(if negative { -cents } else { cents })
}
fn fmt(cents: i128) -> String {
    format!(
        "{}{}.{:02}",
        if cents < 0 { "-" } else { "" },
        cents.abs() / 100,
        cents.abs() % 100
    )
}
fn rounded(cents: i128) -> i128 {
    if cents < 0 {
        -rounded(-cents)
    } else {
        ((cents + 50) / 100) * 100
    }
}
fn rate(cents: i128, numerator: i128, denominator: i128) -> i128 {
    // Keep sub-cent precision until the whole-dollar half-up boundary.
    let scaled = cents * numerator;
    if scaled < 0 {
        -rate(-cents, numerator, denominator)
    } else {
        ((scaled + denominator * 50) / (denominator * 100)) * 100
    }
}

/// Check one directory of proposed return PDFs and explicitly supplied W-2 source documents.
pub fn run(workspace: &Path, year: u16, folder: Option<&str>, sources: &[String], complete: bool) -> Result<Report> {
    let mut report = Report::new(year, complete);
    if year != 2025 {
        report.add(
            "review",
            "unsupported_year",
            "return",
            None,
            "tax_year",
            "Only 2025 validation rules are implemented; no checks were run for this year.".into(),
        );
        report.finish();
        return Ok(report);
    }
    let folder = folder.map(str::to_owned).unwrap_or_else(|| format!("{year}/forms"));
    let dir = crate::documents::contained(workspace, Path::new(&folder))?;
    let root = workspace.canonicalize()?;
    let mut paths = std::fs::read_dir(&dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    let mut forms = vec![];
    for path in paths {
        if !path.extension().is_some_and(|s| s.eq_ignore_ascii_case("pdf")) {
            continue;
        }
        let path = crate::documents::contained(&root, &path)?;
        let file = path.strip_prefix(&root)?.display().to_string();
        let bytes = std::fs::read(&path)?;
        let fields = match nii_pdf::acroform::normalize(&bytes).and_then(|bytes| nii_pdf::acroform::read_fields(&bytes))
        {
            Ok(fields) if !fields.is_empty() => fields,
            result => {
                report.add(
                    "error",
                    "unreadable_form",
                    &file,
                    None,
                    "PDF fields",
                    format!(
                        "Cannot inspect editable fields (corrupt or flattened PDF): {}",
                        result
                            .err()
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| "no AcroForm fields".into())
                    ),
                );
                continue;
            }
        };
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        let kind = KINDS
            .iter()
            .find(|k| name == **k || name.starts_with(&format!("{k}-")) || name.starts_with(&format!("{k}_")))
            .map(|k| (*k).to_string());
        if !fields.iter().any(present) && kind.as_deref() != Some("f1040") {
            continue;
        }
        match kind {
            Some(kind) => {
                report.forms_checked.push(file.clone());
                forms.push(Form { file, kind, fields });
            }
            None => report.add(
                "review",
                "unsupported_form",
                &file,
                None,
                "coverage",
                "No rule set for this populated form. Review it separately; it has not been validated.".into(),
            ),
        }
    }
    let mut w2s = vec![];
    let mut seen = BTreeSet::new();
    let mut sources_ok = true;
    for source in sources {
        let path = crate::documents::contained(&root, Path::new(source))?;
        if !seen.insert(path.clone()) {
            report.add(
                "error",
                "duplicate_w2_source",
                source,
                None,
                "source",
                "The same W-2 source path was supplied more than once; it was not counted twice.".into(),
            );
            sources_ok = false;
            continue;
        }
        match read_w2(&path) {
            Ok(w2) => {
                report.w2_sources_checked.push(source.clone());
                w2s.push(w2);
            }
            Err(e) => {
                sources_ok = false;
                report.add("review", "w2_source_unreadable", source, None, "source", format!("Cannot extract this W-2 unambiguously: {e}. Supply a labelled transcription or review the source manually; its amounts were not assumed zero."));
            }
        }
    }
    report.w2_inventory_complete = complete && sources_ok;
    validate_forms(&forms, &w2s, report)
}

fn read_w2(path: &Path) -> Result<W2> {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
        let bytes = nii_pdf::acroform::normalize(&std::fs::read(path)?)?;
        let fields = nii_pdf::acroform::read_fields(&bytes)?;
        return w2_from_fields(&fields);
    }
    anyhow::ensure!(
        matches!(path.extension().and_then(|e| e.to_str()), Some("txt" | "md")),
        "supported W-2 sources are fillable PDFs and labelled .txt/.md transcriptions"
    );
    let text = std::fs::read_to_string(path)?;
    anyhow::ensure!(
        !Regex::new(r"(?im)statutory\s+employee\s*:\s*(?:checked|yes|true|1)\b")?.is_match(&text),
        "statutory employee wage placement requires manual review"
    );
    let year_re = Regex::new(r"(?i)\btax\s+year\s*[: ]\s*(\d{4})\b")?;
    let years: BTreeSet<_> = year_re.captures_iter(&text).map(|m| m[1].to_string()).collect();
    anyhow::ensure!(
        years.len() == 1 && years.contains("2025"),
        "labelled transcription must explicitly identify tax year 2025"
    );
    let owner_re = Regex::new(r"(?im)^\s*Employee[^\n]*?\bSSN\s*[: ]\s*([0-9]{3}-?[0-9]{2}-?[0-9]{4})\s*$")?;
    let owners: Vec<_> = owner_re.captures_iter(&text).collect();
    anyhow::ensure!(owners.len() == 1, "expected one Employee ... SSN line");
    let owner = tin(&owners[0][1]).context("invalid employee identifier")?;
    let amount = |number: u8, label: &str, optional: bool| -> Result<i128> {
        let re = Regex::new(&format!(r"(?im)^\s*(?:Box\s+)?{number}[.\s]+{label}\s*:\s*([^\r\n]+)$"))?;
        let hits: Vec<_> = re.captures_iter(&text).collect();
        if optional && hits.is_empty() {
            let box_re = Regex::new(&format!(r"(?im)^\s*(?:Box\s+)?{number}(?:[.\s:]|$)"))?;
            anyhow::ensure!(
                !box_re.is_match(&text),
                "box {number} is present but its label is unsupported"
            );
            return Ok(0);
        }
        anyhow::ensure!(hits.len() == 1, "missing or conflicting box {number}");
        money(hits[0][1].trim()).with_context(|| format!("invalid box {number} amount"))
    };
    let wages = amount(1, r"wages(?:,?\s*tips(?:,?\s*other\s*compensation)?)?", false)?;
    let withheld = amount(2, r"federal\s+income\s+tax\s+withheld", false)?;
    let social = amount(3, r"social\s+security\s+wages", false)? + amount(7, r"social\s+security\s+tips", true)?;
    anyhow::ensure!(
        wages >= 0 && withheld >= 0 && social >= 0,
        "negative W-2 amounts require manual review"
    );
    Ok(W2 {
        owner,
        wages,
        withheld,
        social,
    })
}

fn w2_from_fields(fields: &[Field]) -> Result<W2> {
    anyhow::ensure!(
        !fields.iter().any(|f| ticked(f)
            && norm(f.description.as_deref().unwrap_or(""))
                .to_ascii_lowercase()
                .contains("statutory employee")),
        "statutory employee wage placement requires manual review"
    );
    let unique = |prefix: &str, optional: bool| -> Result<String> {
        let matching: Vec<_> = fields
            .iter()
            .filter(|f| {
                norm(f.description.as_deref().unwrap_or(""))
                    .to_ascii_lowercase()
                    .starts_with(prefix)
            })
            .collect();
        anyhow::ensure!(
            !matching.is_empty(),
            "missing {prefix} field in this W-2 layout; absence was not assumed zero"
        );
        let values: BTreeSet<_> = matching
            .into_iter()
            .filter_map(|f| f.value.as_deref())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned)
            .collect();
        if optional && values.is_empty() {
            return Ok("0".into());
        }
        anyhow::ensure!(
            values.len() == 1,
            "missing or conflicting {prefix} values; only a single W-2 (including its duplicate copies) is supported per source file"
        );
        Ok(values.into_iter().next().unwrap())
    };
    let owner = tin(&unique("a. employee's social security number", false)?)
        .context("employee identifier is not nine digits")?;
    anyhow::ensure!(
        tin(&unique("b. employer identification number", false)?).is_some(),
        "one unambiguous employer identifier is required per W-2 source file"
    );
    anyhow::ensure!(
        unique("for calendar year", false)?.trim() == "2025",
        "W-2 calendar year must be explicitly reported as 2025; otherwise source-period verification needs manual review"
    );
    let wages = money(&unique("1. wages, tips", true)?).context("invalid box 1 amount")?;
    let withheld = money(&unique("2. federal income tax withheld", true)?).context("invalid box 2 amount")?;
    let social = money(&unique("3. social security wages", true)?).context("invalid box 3 amount")?
        + money(&unique("7. social security tips", true)?).context("invalid box 7 amount")?;
    anyhow::ensure!(
        wages >= 0 && withheld >= 0 && social >= 0,
        "negative W-2 amounts require manual review"
    );
    Ok(W2 {
        owner,
        wages,
        withheld,
        social,
    })
}

struct Checker<'a> {
    report: Report,
    mapped: BTreeMap<(String, String, Option<u32>), Option<&'a Field>>,
    computed: BTreeMap<(String, String), i128>,
}
impl<'a> Checker<'a> {
    fn field(&mut self, form: &'a Form, selector: &str, page: Option<u32>) -> Option<&'a Field> {
        let key = (form.file.clone(), selector.to_string(), page);
        if let Some(hit) = self.mapped.get(&key) {
            return *hit;
        }
        let hits: Vec<_> = form
            .fields
            .iter()
            .filter(|f| page.is_none_or(|p| f.page == Some(p)))
            .filter(|f| {
                selector.split("||").all(|s| {
                    label_matches(
                        &norm(f.description.as_deref().unwrap_or(""))
                            .replace("Properties. Column:", "Column: Properties."),
                        s,
                    )
                })
            })
            .filter(|f| matches!(f.kind, FieldKind::Text { .. }))
            .collect();
        let hit = if hits.len() == 1 {
            Some(hits[0])
        } else {
            self.report.add(
                "review",
                "unresolved_field",
                &form.file,
                None,
                selector,
                format!(
                    "Rule matched {} text fields; this field layout cannot be checked safely.",
                    hits.len()
                ),
            );
            None
        };
        self.mapped.insert(key, hit);
        hit
    }
    fn raw_number(&mut self, form: &'a Form, selector: &str) -> Option<Money> {
        let field = self.field(form, selector, None)?;
        match field.value.as_deref().filter(|s| !s.trim().is_empty()) {
            None => Some(Money {
                cents: 0,
                present: false,
            }),
            Some(value) => match money(value) {
                Some(cents) => Some(Money { cents, present: true }),
                None => {
                    self.report.add("error", "invalid_amount", &form.file, Some(field), selector, "Enter a finite decimal dollar amount with at most two decimal places; this value was not treated as zero.".into());
                    None
                }
            },
        }
    }
    fn number(&mut self, form: &'a Form, selector: &str) -> Option<Money> {
        let mut value = self.raw_number(form, selector)?;
        let field = self.field(form, selector, None)?;
        if let Some(expected) = self.computed.get(&(form.file.clone(), field.name.clone())) {
            value.cents = *expected;
        }
        Some(value)
    }
    fn check(&mut self, form: &'a Form, selector: &str, expected: i128, code: &'static str) {
        let Some(actual) = self.raw_number(form, selector) else {
            return;
        };
        self.report.checks_run += 1;
        let field = self.field(form, selector, None);
        if let Some(field) = field {
            self.computed.insert((form.file.clone(), field.name.clone()), expected);
        }
        if !actual.present && expected != 0 {
            self.report.add(
                "error",
                "missing_amount",
                &form.file,
                field,
                selector,
                format!("This line is blank; the referenced lines require {}.", fmt(expected)),
            );
        } else if (actual.cents - expected).abs() > TOLERANCE {
            self.report.add(
                "error",
                code,
                &form.file,
                field,
                selector,
                format!(
                    "Expected {} from the referenced lines/source, found {}.",
                    fmt(expected),
                    fmt(actual.cents)
                ),
            );
        }
    }
    fn sum(&mut self, form: &'a Form, total: &str, terms: &[&str]) {
        let values: Option<Vec<_>> = terms.iter().map(|s| self.number(form, s).map(|v| v.cents)).collect();
        if let Some(values) = values {
            self.check(form, total, values.iter().sum(), "arithmetic_mismatch");
        }
    }
    fn diff(&mut self, form: &'a Form, total: &str, left: &str, right: &str, floor: bool) {
        if let (Some(a), Some(b)) = (self.number(form, left), self.number(form, right)) {
            self.check(
                form,
                total,
                if floor {
                    (a.cents - b.cents).max(0)
                } else {
                    a.cents - b.cents
                },
                "arithmetic_mismatch",
            );
        }
    }
    fn multiply(&mut self, form: &'a Form, total: &str, base: &str, n: i128, d: i128) {
        if let Some(base) = self.number(form, base) {
            self.check(form, total, rate(base.cents, n, d), "arithmetic_mismatch");
        }
    }
    fn required(&mut self, form: &'a Form, selector: &str, identifier: bool) -> Option<String> {
        let field = self.field(form, selector, Some(1))?;
        self.report.checks_run += 1;
        if !present(field) {
            self.report.add(
                "error",
                "missing_identity",
                &form.file,
                Some(field),
                selector,
                "Required identifying information is blank.".into(),
            );
            return None;
        }
        if identifier {
            let value = field.value.as_deref().unwrap_or("").trim();
            let description = field.description.as_deref().unwrap_or("");
            if (value.eq_ignore_ascii_case("NRA") && description.contains("Spouse's"))
                || (value.eq_ignore_ascii_case("DIED") && description.contains("Column: Dependent"))
            {
                self.report.add("review", "identifier_exception", &form.file, Some(field), selector,
                    "This identifier marker can require a special filing exception and supporting records. Verify eligibility manually; it was not treated as an ordinary identifier.".into());
                return None;
            }
            let id = tin(field.value.as_deref().unwrap_or(""));
            if id.is_none() {
                self.report.add("error","invalid_identifier_format",&form.file,Some(field),selector,"Expected nine digits (SSN/ITIN); issuance and eligibility are not validated. Identifiers are not echoed in this report.".into());
            }
            id
        } else {
            field.value.clone()
        }
    }
}

fn one<'a>(forms: &'a [Form], kind: &str) -> Option<&'a Form> {
    let mut hits = forms.iter().filter(|f| f.kind == kind);
    let first = hits.next()?;
    hits.next().is_none().then_some(first)
}
fn validate_forms(forms: &[Form], w2s: &[W2], report: Report) -> Result<Report> {
    let mut c = Checker {
        report,
        mapped: BTreeMap::new(),
        computed: BTreeMap::new(),
    };
    for kind in KINDS.iter().filter(|k| !matches!(**k, "f1040sc" | "f1040sse")) {
        if forms.iter().filter(|f| f.kind == **kind).count() > 1 {
            c.report.add(if *kind == "f1040se" { "review" } else { "error" },"duplicate_summary_form",kind,None,"return set","More than one populated copy of this summary form was supplied. Multiple Schedule E pages can be valid, but are not merged by this version. Select/review the proposed return copies; totals were not guessed.".into());
        }
    }
    let main = one(forms, "f1040");
    if main.is_none() {
        c.report.add(
            "error",
            "missing_main_return",
            "f1040",
            None,
            "return set",
            "Exactly one readable Form 1040 is required.".into(),
        );
    }
    let mut adults = BTreeSet::new();
    if let Some(f) = main {
        let year_ok = f.fields.iter().any(|field| {
            field
                .description
                .as_deref()
                .is_some_and(|s| s.contains("Jan. 1–Dec. 31, 2025"))
        });
        if !year_ok {
            c.report.add(
                "review",
                "unverified_form_year",
                &f.file,
                None,
                "form revision",
                "Cannot confirm the 2025 Form 1040 field layout. No year-specific arithmetic rules were applied."
                    .into(),
            );
            c.report.finish();
            return Ok(c.report);
        }
        if f.fields.iter().any(|field| {
            present(field)
                && field.description.as_deref().is_some_and(|d| {
                    d.contains("tax year beginning")
                        || d.contains(", 2025, ending.")
                        || d.contains(", 20. 2 digit year.")
                })
        }) {
            c.report.add("review", "fiscal_year_unsupported", &f.file, None, "tax period",
                "Fiscal-year date fields are populated; calendar-year checks were not applied. Review the tax period and applicable rules manually.".into());
            c.report.finish();
            return Ok(c.report);
        }
        adults = identities(&mut c, f);
        if c.report.w2_inventory_complete {
            c.check(
                f,
                "1a. Total amount from Form(s) W-2",
                rounded(w2s.iter().map(|w| w.wages).sum()),
                "source_w2_wages_mismatch",
            );
            c.check(
                f,
                "25. Federal income tax withheld from: a.",
                rounded(w2s.iter().map(|w| w.withheld).sum()),
                "source_w2_withholding_mismatch",
            );
        }
        main_math(&mut c, f);
    }
    let mut se_owners = BTreeSet::new();
    for f in forms {
        let marker = match f.kind.as_str() {
            "f1040sc" => Some("during 2025"),
            "f1040sse" => Some("for 2025"),
            "f1040se" => Some("payments in 2025"),
            "f1040s8" => Some("by $2,200"),
            "f8995" => Some("$197,300"),
            _ => None,
        };
        if let Some(marker) = marker
            && !f
                .fields
                .iter()
                .any(|field| norm(field.description.as_deref().unwrap_or("")).contains(marker))
        {
            c.report.add("review", "unverified_form_year", &f.file, None, "form revision",
                "The expected 2025 layout/annual figures could not be confirmed. Year-specific rules were skipped for this form.".into());
            continue;
        }
        if f.kind != "f1040" {
            let (name, id) = match f.kind.as_str() {
                "f1040sc" => ("Name of proprietor.", "Social security number (S S N)."),
                "f1040sse" => (
                    "Name of person with self-employment income",
                    "Social security number of person with self-employment income.",
                ),
                "f8995" => ("Name(s) shown on return.", "Your taxpayer identification number."),
                _ => ("Name(s) shown on", "Your social security number."),
            };
            c.required(f, name, false);
            if let Some(id) = c.required(f, id, true) {
                let adult_ids_complete = main.is_some_and(|main| {
                    !c.report.findings.iter().any(|issue| {
                        issue.form == main.file
                            && (issue.line == "Filing Status"
                                || issue.line == "Your social security number."
                                || issue.line == "Spouse's social security number.")
                    })
                });
                if adult_ids_complete && !adults.is_empty() && !adults.contains(&id) {
                    c.report.add(
                        "error",
                        "owner_not_on_return",
                        &f.file,
                        None,
                        "owner identifier",
                        "This form's owner identifier does not match either adult on Form 1040.".into(),
                    );
                }
                if f.kind == "f1040sse" && !se_owners.insert(id) {
                    c.report.add(
                        "error",
                        "duplicate_se_owner",
                        &f.file,
                        None,
                        "owner identifier",
                        "Combine a person's businesses on one Schedule SE; each spouse needs a separate Schedule SE."
                            .into(),
                    );
                }
            }
        }
        match f.kind.as_str() {
            "f1040sc" => schedule_c(&mut c, f),
            "f1040sse" => schedule_se(&mut c, f, w2s),
            "f1040s1" => schedule_1(&mut c, f),
            "f1040s2" => schedule_2(&mut c, f),
            "f8995" => qbi(&mut c, f, main),
            "f1040se" => rental(&mut c, f),
            "f1040s8" => {
                c.sum(
                    f,
                    "2d. Add lines",
                    &["2a. Enter income", "2b. Enter the amounts", "2c. Enter the amount"],
                );
                c.sum(
                    f,
                    "3. Add lines",
                    &["1. Enter the amount from line 11a", "2d. Add lines"],
                );
                c.multiply(f, "5. Multiply line 4", "4. Number of qualifying children", 2200, 1);
                c.multiply(f, "7. Multiply line 6", "6. Number of other dependents", 500, 1);
                c.sum(f, "8. Add lines", &["5. Multiply line 4", "7. Multiply line 6"]);
                // Eligibility, phaseout and Credit Limit Worksheet A require additional facts.
            }
            "f1040sb" => {
                c.diff(
                    f,
                    "4. Subtract line 3",
                    "2. Add the amounts",
                    "3. Excludable interest",
                    false,
                );
                c.report.add("warning","review_optional_schedule_b",&f.file,None,"necessity","Check whether Schedule B is required (including foreign-account/trust questions). A small interest amount alone does not establish that it is unnecessary.".into());
            }
            "f1040s3" => {}
            _ => {}
        }
    }
    if let Some(f) = main {
        transfers(&mut c, forms, f);
    }
    if forms.iter().any(|f| f.kind == "f1040sse") && !c.report.w2_inventory_complete {
        c.report.add("review","incomplete_w2_inventory","return",None,"Schedule SE wage ownership","Supply all W-2 sources and confirm w2_complete before validating each SE owner's wages. Absence of a supplied W-2 is not proof of zero wages.".into());
    }
    c.report.finish();
    Ok(c.report)
}

fn identities<'a>(c: &mut Checker<'a>, f: &'a Form) -> BTreeSet<String> {
    let statuses = [
        "Filing Status. Check only one box. Single.",
        "Married filing jointly",
        "Married filing separately",
        "Head of household",
        "Qualifying surviving spouse",
    ];
    let selected: Vec<_> = f
        .fields
        .iter()
        .filter(|field| {
            statuses
                .iter()
                .any(|s| norm(field.description.as_deref().unwrap_or("")).starts_with(s))
        })
        .filter(|field| ticked(field))
        .collect();
    c.report.checks_run += 1;
    if selected.len() != 1 {
        c.report.add(
            "error",
            "filing_status_count",
            &f.file,
            None,
            "Filing Status",
            format!("Select exactly one filing status; found {} selected.", selected.len()),
        );
    }
    let joint = selected.iter().any(|field| {
        field
            .description
            .as_deref()
            .unwrap_or("")
            .starts_with("Married filing jointly")
    });
    let separate = selected.iter().any(|field| {
        field
            .description
            .as_deref()
            .unwrap_or("")
            .starts_with("Married filing separately")
    });
    let mut ids = BTreeSet::new();
    c.required(f, "Your first name and middle initial.", false);
    if let Some(id) = c.required(f, "Your social security number.", true) {
        ids.insert(id);
    }
    if joint {
        c.required(f, "spouse's first name and middle initial.", false);
    }
    let spouse_header = f.fields.iter().any(|field| {
        field
            .description
            .as_deref()
            .is_some_and(|d| d.contains("spouse's first name and middle initial."))
            && present(field)
    });
    if (joint || separate || spouse_header)
        && let Some(id) = c.required(f, "Spouse's social security number.", true)
        && !ids.insert(id)
    {
        c.report.add(
            "error",
            "duplicate_identifier",
            &f.file,
            None,
            "Spouse identifier",
            "Taxpayer and spouse identifiers must differ.".into(),
        );
    }
    let adults = ids.clone();
    let lasts: Vec<_> = f
        .fields
        .iter()
        .filter(|field| field.page == Some(1) && norm(field.description.as_deref().unwrap_or("")) == "Last name.")
        .collect();
    if lasts.len() == 2 {
        for field in lasts.iter().take(if joint { 2 } else { 1 }) {
            if !present(field) {
                c.report.add(
                    "error",
                    "missing_identity",
                    &f.file,
                    Some(field),
                    "Last name",
                    "Required last name is blank.".into(),
                );
            }
        }
    } else {
        c.report.add(
            "review",
            "unresolved_field",
            &f.file,
            None,
            "Last name",
            "Cannot identify the taxpayer/spouse last-name fields safely.".into(),
        );
    }
    c.required(f, "Home address (number and street)", false);
    for column in 1..=4 {
        let tag = format!("Column: Dependent {column}.");
        let rows: Vec<_> = f
            .fields
            .iter()
            .filter(|field| field.description.as_deref().is_some_and(|d| d.contains(&tag)))
            .collect();
        if !rows.iter().any(|field| present(field)) {
            continue;
        }
        for label in ["(1) First name.", "(2) Last name.", "(4) Relationship."] {
            c.required(f, &format!("{label} {tag}"), false);
        }
        if let Some(id) = c.required(f, &format!("(3) S S N. {tag}"), true)
            && !ids.insert(id)
        {
            c.report.add(
                "error",
                "duplicate_identifier",
                &f.file,
                None,
                &tag,
                "Dependent identifiers must be distinct from the adults and other dependents.".into(),
            );
        }
        if rows
            .iter()
            .filter(|field| ticked(field) && field.description.as_deref().is_some_and(|d| d.contains("(7) Credits.")))
            .count()
            > 1
        {
            c.report.add(
                "error",
                "dependent_credit_conflict",
                &f.file,
                None,
                &tag,
                "A dependent cannot have both child and other-dependent credit boxes selected.".into(),
            );
        }
    }
    adults
}

fn main_math<'a>(c: &mut Checker<'a>, f: &'a Form) {
    c.sum(
        f,
        "1z. Add lines",
        &[
            "1a. Total amount",
            "1b. Household employee",
            "1c. Tip income",
            "1d. Medicaid waiver",
            "1e. Taxable dependent",
            "1f. Employer-provided",
            "1g. Wages from",
            "1h. and amount",
        ],
    );
    c.sum(
        f,
        "9. Add lines 1z",
        &[
            "1z. Add lines",
            "2b. Taxable interest",
            "3b. Ordinary dividends",
            "4b. Taxable amount",
            "5b. Taxable amount",
            "6b. Taxable amount",
            "7a. Capital gain",
            "8. Additional income",
        ],
    );
    c.diff(
        f,
        "11a. Subtract line 10",
        "9. Add lines 1z",
        "10. Adjustments to income",
        false,
    );
    c.sum(f, "11b. Amount from line 11a", &["11a. Subtract line 10"]);
    c.sum(
        f,
        "14. Add lines",
        &[
            "12e. Standard deduction",
            "13a. Qualified business",
            "13b. Additional deductions",
        ],
    );
    c.diff(
        f,
        "15. Subtract line 14",
        "11b. Amount from line 11a",
        "14. Add lines",
        true,
    );
    c.sum(f, "18. Add lines", &["16. Amount.", "17. Amount from Schedule 2"]);
    c.sum(
        f,
        "21. Add lines",
        &["19. Child tax credit", "20. Amount from Schedule 3"],
    );
    c.diff(f, "22. Subtract line 21", "18. Add lines", "21. Add lines", true);
    c.sum(f, "24. Add lines", &["22. Subtract line 21", "23. Other taxes"]);
    c.sum(
        f,
        "25d. Add lines",
        &[
            "25. Federal income tax withheld from: a.",
            "25b. Form(s) 1099.",
            "25c. Other forms",
        ],
    );
    c.sum(
        f,
        "32. Add lines",
        &[
            "27a. Earned income credit",
            "28. Amount.",
            "29. American opportunity",
            "30. Refundable adoption",
            "31. Amount from Schedule 3",
        ],
    );
    c.sum(
        f,
        "33. Add lines",
        &[
            "25d. Add lines",
            "26. 2025 estimated tax payments and amount",
            "32. Add lines",
        ],
    );
    c.diff(f, "Refund. 34.", "33. Add lines", "24. Add lines", true);
    // IRS 2025 instructions, line 38: a declared penalty reduces the refund
    // allocation or increases line 37, including when it exceeds overpayment.
    if let (Some(tax), Some(pay), Some(penalty), Some(refund), Some(forward)) = (
        c.number(f, "24. Add lines"),
        c.number(f, "33. Add lines"),
        c.number(f, "38. Estimated tax penalty"),
        c.number(f, "35a. Amount."),
        c.number(f, "36. Amount of line 34"),
    ) {
        for selector in [
            "Refund. 34.",
            "35a. Amount.",
            "36. Amount of line 34",
            "Amount You Owe. 37.",
            "38. Estimated tax penalty",
        ] {
            if let Some(value) = c.raw_number(f, selector) {
                c.report.checks_run += 1;
                if value.cents < 0 {
                    let field = c.field(f, selector, None);
                    c.report.add(
                        "error",
                        "negative_payment_or_refund",
                        &f.file,
                        field,
                        selector,
                        "Refund, amount applied forward, balance owed and penalty lines cannot be negative.".into(),
                    );
                }
            }
        }
        c.check(
            f,
            "Amount You Owe. 37.",
            (tax.cents + penalty.cents - pay.cents).max(0),
            "arithmetic_mismatch",
        );
        let available = (pay.cents - tax.cents - penalty.cents).max(0);
        c.report.checks_run += 1;
        if (refund.cents + forward.cents - available).abs() > TOLERANCE
            || (available > 0 && !refund.present && !forward.present)
        {
            let field = c.field(f, "35a. Amount.", None);
            c.report.add(
                "error",
                "refund_allocation_mismatch",
                &f.file,
                field,
                "35a / 36",
                format!(
                    "Refund plus amount applied forward must equal {} after the declared penalty. Found {}.",
                    fmt(available),
                    fmt(refund.cents + forward.cents)
                ),
            );
        }
        if penalty.cents > 0 {
            c.report.add("warning","penalty_not_recomputed",&f.file,None,"38. Estimated tax penalty",
                "Refund/balance arithmetic includes this reported penalty. The penalty's eligibility and payment-date calculation were not recomputed.".into());
        }
    }
}

fn schedule_c<'a>(c: &mut Checker<'a>, f: &'a Form) {
    c.diff(
        f,
        "3. Subtract line 2",
        "1. Gross receipts or sales.",
        "2. Returns and allowances.",
        false,
    );
    c.diff(
        f,
        "5. Gross profit",
        "3. Subtract line 2",
        "4. Cost of goods sold",
        false,
    );
    c.sum(f, "7. Gross income", &["5. Gross profit", "6. Other income"]);
    c.sum(
        f,
        "28. Total expenses",
        &[
            "8. Advertising.",
            "9. Car and truck",
            "10. Commissions",
            "11. Contract labor",
            "12. Depletion.",
            "13. Depreciation",
            "14. Employee benefit",
            "15. Insurance",
            "16. Interest",
            "16b. Other.",
            "17. Legal",
            "18. Office expense",
            "19. Pension",
            "20. Rent or lease",
            "20b. Other business",
            "21. Repairs",
            "22. Supplies",
            "23. Taxes",
            "24. Travel and meals",
            "24b. Deductible meals",
            "25. Utilities.",
            "26. Wages",
            "27a. Energy efficient",
            "27b. Other expenses",
        ],
    );
    c.diff(
        f,
        "29. Tentative profit",
        "7. Gross income",
        "28. Total expenses",
        false,
    );
    c.diff(
        f,
        "31. Net profit",
        "29. Tentative profit",
        "30. Use the Simplified Method Worksheet",
        false,
    );
}

fn schedule_se<'a>(c: &mut Checker<'a>, f: &'a Form, w2s: &[W2]) {
    if let (Some(a), Some(b), Some(d)) = (
        c.number(f, "1a. Net farm profit"),
        c.number(f, "1b. Open parenthesis"),
        c.number(f, "2. Net profit or (loss) from Schedule C"),
    ) {
        c.check(
            f,
            "3. Combine lines",
            a.cents - b.cents.abs() + d.cents,
            "arithmetic_mismatch",
        );
        if b.cents != 0 {
            c.report.add(
                "review",
                "se_special_method",
                &f.file,
                None,
                "Conservation Reserve Program",
                "CRP income can change the filing threshold; verify the Schedule SE exceptions.".into(),
            );
        }
    }
    if let Some(n) = c.number(f, "3. Combine lines") {
        c.check(
            f,
            "4a. If line 3",
            if n.cents > 0 {
                rate(n.cents, 9235, 10000)
            } else {
                n.cents
            },
            "arithmetic_mismatch",
        );
    }
    c.sum(f, "4c. Combine lines", &["4a. If line 3", "4b. If you elect"]);
    if let Some(n) = c.number(f, "5a. Enter your church") {
        let expected = rate(n.cents, 9235, 10000);
        c.check(
            f,
            "5b. Multiply line 5a",
            if expected < 10000 { 0 } else { expected },
            "arithmetic_mismatch",
        );
    }
    c.sum(f, "6. Add lines", &["4c. Combine lines", "5b. Multiply line 5a"]);
    if c.number(f, "4c. Combine lines").is_some_and(|m| m.cents < 40000)
        && c.number(f, "5a. Enter your church").is_some_and(|m| m.cents == 0)
    {
        if c.number(f, "12. Self-employment tax").is_some_and(|m| m.cents != 0) {
            c.report.add(
                "review",
                "se_below_threshold",
                &f.file,
                None,
                "12. Self-employment tax",
                "Earnings are below the normal filing threshold; verify exceptions before charging SE tax.".into(),
            );
        }
        return;
    }
    if c.report.w2_inventory_complete {
        let owner = c
            .field(
                f,
                "Social security number of person with self-employment income.",
                Some(1),
            )
            .and_then(|field| field.value.as_deref())
            .and_then(tin);
        if let Some(owner) = owner {
            let social = rounded(w2s.iter().filter(|w| w.owner == owner).map(|w| w.social).sum());
            c.check(f, "8a. Total social security wages", social, "se_owner_w2_mismatch");
        }
    }
    c.sum(
        f,
        "8d. Add lines",
        &[
            "8a. Total social security wages",
            "8b. Unreported tips",
            "8c. Wages subject",
        ],
    );
    if let Some(wages) = c.number(f, "8d. Add lines") {
        c.check(
            f,
            "9. Subtract line 8d",
            (17_610_000 - wages.cents).max(0),
            "arithmetic_mismatch",
        );
    }
    if let (Some(a), Some(b)) = (c.number(f, "6. Add lines"), c.number(f, "9. Subtract line 8d")) {
        c.check(
            f,
            "10. Multiply the smaller",
            rate(a.cents.min(b.cents), 124, 1000),
            "arithmetic_mismatch",
        );
    }
    c.multiply(f, "11. Multiply line 6", "6. Add lines", 29, 1000);
    c.sum(
        f,
        "12. Self-employment tax",
        &["10. Multiply the smaller", "11. Multiply line 6"],
    );
    c.multiply(f, "13. Deduction for one-half", "12. Self-employment tax", 1, 2);
}

fn schedule_1<'a>(c: &mut Checker<'a>, f: &'a Form) {
    c.sum(
        f,
        "10. Combine lines",
        &[
            "1. Taxable refunds",
            "2a. Alimony received",
            "3. Business income",
            "4. Amount.",
            "5. Rental real estate",
            "6. Farm income",
            "7. Amount.",
            "9. Total other income",
        ],
    );
    c.sum(
        f,
        "26. Add lines",
        &[
            "11. Educator expenses",
            "12. Certain business expenses",
            "13. Health savings",
            "14. Amount.",
            "15. Deductible part",
            "16. Self-employed",
            "17. Self-employed health",
            "18. Penalty",
            "19a. Alimony paid",
            "20. Amount.",
            "21. Student loan",
            "22. Reserved",
            "23. Archer",
            "25. Total other adjustments",
        ],
    );
}
fn schedule_2<'a>(c: &mut Checker<'a>, f: &'a Form) {
    c.sum(f, "3. Add lines", &["1z. Add lines", "2. Alternative minimum"]);
    c.sum(
        f,
        "7. Total additional",
        &["5. Social security", "6. Uncollected social"],
    );
    c.sum(
        f,
        "21. Add lines",
        &[
            "4. Amount.",
            "7. Total additional",
            "8. Amount.",
            "9. Household",
            "10. Reserved",
            "11. Additional Medicare",
            "12. Net investment",
            "13. Uncollected social",
            "14. Interest on tax",
            "15. Interest on the deferred",
            "16. Recapture",
            "18. Total additional",
            "19. Recapture",
        ],
    );
}
fn qbi<'a>(c: &mut Checker<'a>, f: &'a Form, main: Option<&'a Form>) {
    c.sum(
        f,
        "2. Total qualified business",
        &[
            "Row: i. Column: (c)",
            "Row: i i. Column: (c)",
            "Row: i i i. Column: (c)",
            "Row: i v. Column: (c)",
            "Row: v. Column: (c)",
        ],
    );
    for (total, left, right) in [
        (
            "4. Total qualified business",
            "2. Total qualified business",
            "3. Open parenthesis",
        ),
        (
            "8. Total qualified R E I T",
            "6. Qualified R E I T",
            "7. Open parenthesis",
        ),
    ] {
        if let (Some(a), Some(b)) = (c.number(f, left), c.number(f, right)) {
            c.check(f, total, (a.cents - b.cents.abs()).max(0), "arithmetic_mismatch");
        }
    }
    c.multiply(
        f,
        "5. Qualified business income component",
        "4. Total qualified business",
        1,
        5,
    );
    c.multiply(f, "9. R E I T and P T P component", "8. Total qualified R E I T", 1, 5);
    c.sum(
        f,
        "10. Qualified business income deduction before",
        &[
            "5. Qualified business income component",
            "9. R E I T and P T P component",
        ],
    );
    c.diff(
        f,
        "13. Subtract line 12",
        "11. Taxable income before",
        "12. Enter your net capital gain",
        true,
    );
    c.multiply(f, "14. Income limitation", "13. Subtract line 12", 1, 5);
    if let (Some(a), Some(b)) = (
        c.number(f, "10. Qualified business income deduction before"),
        c.number(f, "14. Income limitation"),
    ) {
        c.check(
            f,
            "15. Qualified business income deduction",
            a.cents.min(b.cents),
            "arithmetic_mismatch",
        );
    }
    if let Some(m) = main
        && let (Some(a), Some(b), Some(d)) = (
            c.number(m, "11b. Amount from line 11a"),
            c.number(m, "12e. Standard deduction"),
            c.number(m, "13b. Additional deductions"),
        )
    {
        c.check(
            f,
            "11. Taxable income before",
            (a.cents - b.cents - d.cents).max(0),
            "transfer_mismatch",
        );
    }
}
fn rental<'a>(c: &mut Checker<'a>, f: &'a Form) {
    let mut has_property_loss = false;
    for column in ["A", "B", "C"] {
        if c.number(f, &format!("Row: {column}. Column: Personal Use Days."))
            .is_some_and(|m| m.cents > 0)
        {
            c.report.add(
                "review", "rental_personal_use", &f.file, None, "2. Personal use days",
                "Verify rental/personal expense allocation and dwelling-unit-used-as-a-home limitations; this checker does not establish the allowable allocation.".into(),
            );
        }
        let select = |row: u8| format!("Row: {row}.||Column: Properties. {column}.");
        let terms: Vec<_> = (5..=19).map(select).collect();
        let refs: Vec<_> = terms.iter().map(String::as_str).collect();
        c.sum(f, &select(20), &refs);
        if let (Some(a), Some(b), Some(d)) = (
            c.number(f, &select(3)),
            c.number(f, &select(4)),
            c.number(f, &select(20)),
        ) {
            c.check(f, &select(21), a.cents + b.cents - d.cents, "arithmetic_mismatch");
        }
        has_property_loss |= c.number(f, &select(21)).is_some_and(|m| m.cents < 0);
    }
    for (total, row) in [
        ("23a. Total", 3),
        ("23b. Total", 4),
        ("23c. Total", 12),
        ("23d. Total", 18),
        ("23e. Total", 20),
    ] {
        let terms: Vec<_> = ["A", "B", "C"]
            .iter()
            .map(|col| format!("Row: {row}.||Column: Properties. {col}."))
            .collect();
        c.sum(f, total, &terms.iter().map(String::as_str).collect::<Vec<_>>());
    }
    let positive: Option<Vec<_>> = ["A", "B", "C"]
        .iter()
        .map(|col| {
            c.number(f, &format!("Row: 21.||Column: Properties. {col}."))
                .map(|m| m.cents.max(0))
        })
        .collect();
    if let Some(values) = positive {
        c.check(f, "24. Income.", values.iter().sum(), "arithmetic_mismatch");
    }
    if let (Some(a), Some(b)) = (c.number(f, "24. Income."), c.number(f, "25. Losses.")) {
        c.check(f, "26. Total rental", a.cents - b.cents.abs(), "arithmetic_mismatch");
    }
    // Deductible losses cannot be recomputed without basis/at-risk/passive-use facts.
    if has_property_loss || c.number(f, "25. Losses.").is_some_and(|m| m.cents != 0) {
        c.report.add("review","rental_loss_limits",&f.file,None,"25. Losses","Verify basis, at-risk, passive-loss and personal-use limitations; this checker does not establish loss eligibility.".into());
    }
}

fn transfers<'a>(c: &mut Checker<'a>, forms: &'a [Form], main: &'a Form) {
    for (kind, source, dest) in [
        ("f1040s1", "10. Combine lines", "8. Additional income"),
        ("f1040s1", "26. Add lines", "10. Adjustments to income"),
        ("f1040s2", "3. Add lines", "17. Amount from Schedule 2"),
        ("f1040s2", "21. Add lines", "23. Other taxes"),
        ("f1040s3", "8. Add lines", "20. Amount from Schedule 3"),
        ("f1040s3", "15. Add lines", "31. Amount from Schedule 3"),
        ("f1040s8", "14. Enter the smaller", "19. Child tax credit"),
        (
            "f8995",
            "15. Qualified business income deduction",
            "13a. Qualified business",
        ),
    ] {
        if let Some(f) = one(forms, kind) {
            if let Some(value) = c.number(f, source) {
                c.check(main, dest, value.cents, "transfer_mismatch");
            }
        } else if !forms.iter().any(|f| f.kind == kind) && c.number(main, dest).is_some_and(|v| v.cents != 0) {
            // 8995-A can be an alternative; report manual review rather than calling it missing.
            c.report.add(if kind=="f8995" {"review"}else{"error"},"missing_supporting_form",&main.file,None,dest,format!("This nonzero line needs {kind} (or the applicable alternative). No populated supported copy was found."));
        }
    }
    if let Some(s1) = one(forms, "f1040s1") {
        let profits: Option<Vec<_>> = forms
            .iter()
            .filter(|f| f.kind == "f1040sc")
            .map(|f| c.number(f, "31. Net profit").map(|m| m.cents))
            .collect();
        if forms.iter().any(|f| f.kind == "f1040sc") {
            if let Some(profits) = profits {
                c.check(s1, "3. Business income", profits.iter().sum(), "transfer_mismatch");
            }
        } else if c.number(s1, "3. Business income").is_some_and(|m| m.cents != 0) {
            c.report.add(
                "error",
                "missing_supporting_form",
                &s1.file,
                None,
                "3. Business income",
                "Nonzero business income requires the supporting Schedule C(s).".into(),
            );
        }
        if let Some(e) = one(forms, "f1040se") {
            let parts = [
                "32. Total partnership",
                "37. Total estate",
                "39. Combine columns",
                "40. Net farm rental",
            ];
            let has_other = parts.iter().any(|s| c.number(e, s).is_some_and(|m| m.cents != 0));
            let selector = if has_other {
                "41. Total income"
            } else {
                "26. Total rental"
            };
            if has_other {
                c.report.add(
                    "review",
                    "schedule_e_other_income",
                    &e.file,
                    None,
                    "Part II–V",
                    "Partnership/trust/REMIC/farm income needs additional eligibility and source checks.".into(),
                );
            }
            if let Some(v) = c.number(e, selector) {
                c.check(s1, "5. Rental real estate", v.cents, "transfer_mismatch");
            }
        } else if !forms.iter().any(|f| f.kind == "f1040se")
            && c.number(s1, "5. Rental real estate").is_some_and(|m| m.cents != 0)
        {
            c.report.add(
                "error",
                "missing_supporting_form",
                &s1.file,
                None,
                "5. Rental real estate",
                "Nonzero Schedule E income needs a populated Schedule E.".into(),
            );
        }
    }
    for (kind, dest, source) in [
        ("f1040s1", "15. Deductible part", "13. Deduction for one-half"),
        ("f1040s2", "4. Amount.", "12. Self-employment tax"),
    ] {
        let se: Vec<_> = forms.iter().filter(|f| f.kind == "f1040sse").collect();
        if se.is_empty() {
            continue;
        }
        let values: Option<Vec<_>> = se.iter().map(|f| c.number(f, source).map(|m| m.cents)).collect();
        if let Some(values) = values {
            if let Some(target) = one(forms, kind) {
                c.check(target, dest, values.iter().sum(), "transfer_mismatch");
            } else if values.iter().sum::<i128>() != 0 {
                c.report.add(
                    "error",
                    "missing_supporting_form",
                    kind,
                    None,
                    dest,
                    "Schedule SE amounts need this populated summary schedule.".into(),
                );
            }
        }
    }
    if let Some(s8) = one(forms, "f1040s8")
        && let Some(v) = c.number(main, "11a. Subtract line 10")
    {
        c.check(s8, "1. Enter the amount from line 11a", v.cents, "transfer_mismatch");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // IRS PDF field metadata only, not benchmark answer keys or taxpayer values.
    fn form(kind: &str, suffix: &str) -> Form {
        let all: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/validation-fields-2025.json")).unwrap();
        let fields = all[kind]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| Field {
                name: v["name"].as_str().unwrap().into(),
                description: v["description"].as_str().map(str::to_owned),
                page: v["page"].as_u64().map(|n| n as u32),
                value: None,
                kind: match v["type"].as_str().unwrap() {
                    "text" => FieldKind::Text {
                        max_len: None,
                        multiline: false,
                    },
                    "checkbox" => FieldKind::Checkbox {
                        on_states: v["on_states"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|s| s.as_str().unwrap().into())
                            .collect(),
                    },
                    _ => FieldKind::Other {
                        pdf_type: "other".into(),
                    },
                },
            })
            .collect();
        Form {
            file: format!("{kind}{suffix}.pdf"),
            kind: kind.into(),
            fields,
        }
    }
    fn set(f: &mut Form, selector: &str, value: &str) {
        let hits: Vec<_> = f
            .fields
            .iter()
            .enumerate()
            .filter(|(_, field)| matches!(field.kind, FieldKind::Text { .. }))
            .filter(|(_, field)| {
                selector.split("||").all(|s| {
                    label_matches(
                        &norm(field.description.as_deref().unwrap_or(""))
                            .replace("Properties. Column:", "Column: Properties."),
                        s,
                    )
                })
            })
            .map(|(i, _)| i)
            .collect();
        assert_eq!(hits.len(), 1, "{}: {selector}", f.kind);
        f.fields[hits[0]].value = (!value.is_empty()).then(|| value.into());
    }
    fn check_box(f: &mut Form, prefix: &str) {
        let field = f
            .fields
            .iter_mut()
            .find(|field| field.description.as_deref().is_some_and(|d| d.starts_with(prefix)))
            .unwrap();
        let FieldKind::Checkbox { on_states } = &field.kind else {
            panic!("not a checkbox")
        };
        field.value = Some(on_states[0].clone());
    }
    fn header(f: &mut Form, owner: &str) {
        let (name, id) = match f.kind.as_str() {
            "f1040sc" => ("Name of proprietor.", "Social security number (S S N)."),
            "f1040sse" => (
                "Name of person with self-employment income",
                "Social security number of person with self-employment income.",
            ),
            "f8995" => ("Name(s) shown on return.", "Your taxpayer identification number."),
            _ => ("Name(s) shown on", "Your social security number."),
        };
        // Schedule E repeats the header on page 2; page 1 suffices.
        for (selector, value) in [(name, "Test Household"), (id, owner)] {
            let field = f
                .fields
                .iter_mut()
                .find(|field| {
                    field.page == Some(1) && norm(field.description.as_deref().unwrap_or("")).contains(selector)
                })
                .unwrap();
            field.value = Some(value.into());
        }
    }
    fn main_return(wages: i64, tax: i64, payments: i64) -> Form {
        let mut f = form("f1040", "");
        for (s, v) in [
            ("Your first name and middle initial.", "Test"),
            ("spouse's first name and middle initial.", "Partner"),
            ("Your social security number.", "000111111"),
            ("Spouse's social security number.", "000222222"),
            ("Home address (number and street)", "123 Test Street"),
        ] {
            set(&mut f, s, v);
        }
        for field in &mut f.fields {
            if norm(field.description.as_deref().unwrap_or("")) == "Last name." {
                field.value = Some("Household".into());
            }
        }
        check_box(&mut f, "Married filing jointly");
        for (s, v) in [
            ("1a. Total amount", wages),
            ("1z. Add lines", wages),
            ("9. Add lines 1z", wages),
            ("11a. Subtract line 10", wages),
            ("11b. Amount from line 11a", wages),
            ("12e. Standard deduction", 31500),
            ("14. Add lines", 31500),
            ("15. Subtract line 14", (wages - 31500).max(0)),
            ("16. Amount.", tax),
            ("18. Add lines", tax),
            ("22. Subtract line 21", tax),
            ("24. Add lines", tax),
            ("25. Federal income tax withheld from: a.", payments),
            ("25d. Add lines", payments),
            ("33. Add lines", payments),
            ("Refund. 34.", (payments - tax).max(0)),
            ("35a. Amount.", (payments - tax).max(0)),
            ("Amount You Owe. 37.", (tax - payments).max(0)),
        ] {
            set(&mut f, s, &v.to_string());
        }
        f
    }
    fn report(forms: &[Form], w2s: &[W2], complete: bool) -> Report {
        validate_forms(forms, w2s, Report::new(2025, complete)).unwrap()
    }
    fn assert_clean(r: &Report) {
        assert_eq!(r.status, "checks_passed", "{:#?}", r.findings);
    }
    fn business() -> Vec<Form> {
        let mut main = main_return(180000, 20000, 30000);
        for (s, v) in [
            ("8. Additional income", 10000),
            ("9. Add lines 1z", 190000),
            ("10. Adjustments", 707),
            ("11a. Subtract line 10", 189293),
            ("11b. Amount from line 11a", 189293),
            ("13a. Qualified business", 1859),
            ("14. Add lines", 33359),
            ("15. Subtract line 14", 155934),
            ("23. Other taxes", 1413),
            ("24. Add lines", 21413),
            ("Refund. 34.", 8587),
            ("35a. Amount.", 8587),
        ] {
            set(&mut main, s, &v.to_string());
        }
        let mut sc = form("f1040sc", "-partner");
        header(&mut sc, "000222222");
        for s in [
            "1. Gross receipts",
            "3. Subtract line 2",
            "5. Gross profit",
            "7. Gross income",
            "29. Tentative profit",
            "31. Net profit",
        ] {
            set(&mut sc, s, "10000");
        }
        let mut se = form("f1040sse", "-partner");
        header(&mut se, "000222222");
        for (s, v) in [
            ("2. Net profit", 10000),
            ("3. Combine lines", 10000),
            ("4a. If line 3", 9235),
            ("4c. Combine lines", 9235),
            ("6. Add lines", 9235),
            ("9. Subtract line 8d", 176100),
            ("10. Multiply the smaller", 1145),
            ("11. Multiply line 6", 268),
            ("12. Self-employment tax", 1413),
            ("13. Deduction for one-half", 707),
        ] {
            set(&mut se, s, &v.to_string());
        }
        let mut s1 = form("f1040s1", "");
        header(&mut s1, "000111111");
        for (s, v) in [
            ("3. Business income", 10000),
            ("10. Combine lines", 10000),
            ("15. Deductible part", 707),
            ("26. Add lines", 707),
        ] {
            set(&mut s1, s, &v.to_string());
        }
        let mut s2 = form("f1040s2", "");
        header(&mut s2, "000111111");
        set(&mut s2, "4. Amount.", "1413");
        set(&mut s2, "21. Add lines", "1413");
        let mut q = form("f8995", "");
        header(&mut q, "000111111");
        for (s, v) in [
            ("Row: i. Column: (c)", 9293),
            ("2. Total qualified business", 9293),
            ("4. Total qualified business", 9293),
            ("5. Qualified business income component", 1859),
            ("10. Qualified business income deduction before", 1859),
            ("11. Taxable income before", 157793),
            ("13. Subtract line 12", 157793),
            ("14. Income limitation", 31559),
            ("15. Qualified business income deduction", 1859),
        ] {
            set(&mut q, s, &v.to_string());
        }
        vec![main, sc, se, s1, s2, q]
    }
    fn employee() -> W2 {
        W2 {
            owner: "000111111".into(),
            wages: 18_000_000,
            withheld: 3_000_000,
            social: 17_610_000,
        }
    }

    #[test]
    fn one_field_mutations_of_a_clean_return_detect_specific_errors() {
        assert_clean(&report(&business(), &[employee()], true));
        // Each trial starts from the same clean return and edits one field only.
        let trials = [
            (
                "f1040",
                "Your social security number.",
                "000999999",
                "owner_not_on_return",
            ),
            (
                "f1040",
                "Spouse's social security number.",
                "000111111",
                "duplicate_identifier",
            ),
            ("f1040", "Your first name and middle initial.", "", "missing_identity"),
            (
                "f1040",
                "25. Federal income tax withheld from: a.",
                "31000",
                "source_w2_withholding_mismatch",
            ),
            ("f1040", "1a. Total amount", "180500", "source_w2_wages_mismatch"),
            ("f1040", "8. Additional income", "11000", "transfer_mismatch"),
            ("f1040", "26. 2025 estimated tax payments", "500", "arithmetic_mismatch"),
            ("f1040", "35a. Amount.", "8000", "refund_allocation_mismatch"),
            ("f1040", "Amount You Owe. 37.", "-100", "negative_payment_or_refund"),
            ("f1040sc", "3. Subtract line 2", "10050", "arithmetic_mismatch"),
            (
                "f1040sse",
                "8a. Total social security wages",
                "176100",
                "se_owner_w2_mismatch",
            ),
            ("f1040s1", "3. Business income", "10500", "transfer_mismatch"),
            ("f8995", "Row: i. Column: (c)", "9393", "arithmetic_mismatch"),
            ("f1040", "25d. Add lines", "NaN", "invalid_amount"),
        ];
        for (kind, selector, value, expected) in trials {
            let mut forms = business();
            set(forms.iter_mut().find(|f| f.kind == kind).unwrap(), selector, value);
            let r = report(&forms, &[employee()], true);
            assert_eq!(r.status, "blocked", "{kind}: {selector}={value}: {:#?}", r.findings);
            assert!(
                r.findings.iter().any(|f| f.code == expected),
                "{kind}: {selector}: expected {expected}: {:#?}",
                r.findings
            );
        }
        let mut forms = business();
        check_box(&mut forms[0], "Head of household");
        let r = report(&forms, &[employee()], true);
        assert!(r.findings.iter().any(|f| f.code == "filing_status_count"));
    }

    #[test]
    fn valid_formatting_zero_and_qbi_row_variations_do_not_trigger_errors() {
        let mut forms = business();
        set(&mut forms[0], "Your social security number.", "000-11-1111");
        set(&mut forms[0], "Spouse's social security number.", "000-22-2222");
        set(&mut forms[0], "25d. Add lines", "$30,000.00");
        set(&mut forms[0], "Amount You Owe. 37.", "0");
        let q = forms.iter_mut().find(|f| f.kind == "f8995").unwrap();
        set(q, "Row: i. Column: (c)", "0");
        set(q, "Row: v. Column: (c)", "9,293");
        assert_clean(&report(&forms, &[employee()], true));
    }

    #[test]
    fn rental_losses_and_personal_use_require_review_even_without_a_claimed_loss() {
        let mut e = form("f1040se", "");
        header(&mut e, "000111111");
        set(&mut e, "Row: 9.||Column: Properties. A.", "100");
        set(&mut e, "Row: 20.||Column: Properties. A.", "100");
        set(&mut e, "Row: 21.||Column: Properties. A.", "-100");
        set(&mut e, "23e. Total", "100");
        let m = main_return(0, 0, 0);
        let r = report(&[m, e], &[], true);
        assert_eq!(r.blocking_errors, 0, "{:#?}", r.findings);
        assert!(r.findings.iter().any(|f| f.code == "rental_loss_limits"));
        let mut e = form("f1040se", "");
        header(&mut e, "000111111");
        set(&mut e, "Row: A. Column: Personal Use Days.", "30");
        let r = report(&[main_return(0, 0, 0), e], &[], true);
        assert_eq!(r.blocking_errors, 0, "{:#?}", r.findings);
        assert!(r.findings.iter().any(|f| f.code == "rental_personal_use"));
    }

    #[test]
    fn zero_dollar_blanks_and_no_dependents_are_valid() {
        assert_clean(&report(&[main_return(0, 0, 0)], &[], true));
    }
    #[test]
    fn complete_family_business_return_passes_without_any_answer_key() {
        assert_clean(&report(&business(), &[employee()], true));
    }
    #[test]
    fn incorrect_spouse_wages_that_exhaust_the_cap_are_caught() {
        let mut forms = business();
        let se = forms.iter_mut().find(|f| f.kind == "f1040sse").unwrap();
        for (s, v) in [
            ("8a. Total social security wages", "176100"),
            ("8d. Add lines", "176100"),
            ("9. Subtract line 8d", "0"),
            ("10. Multiply the smaller", "0"),
            ("12. Self-employment tax", "268"),
            ("13. Deduction for one-half", "134"),
        ] {
            set(se, s, v);
        }
        let r = report(&forms, &[employee()], true);
        assert_eq!(r.status, "blocked");
        assert!(r.findings.iter().any(|f| f.code == "se_owner_w2_mismatch"));
        assert!(
            r.findings
                .iter()
                .any(|f| f.line == "12. Self-employment tax" && f.severity == "error")
        );
        assert!(!serde_json::to_string(&r).unwrap().contains("000222222"));
    }
    #[test]
    fn unknown_w2_inventory_never_establishes_zero_owner_wages() {
        let r = report(&business(), &[], false);
        assert_eq!(r.status, "needs_review");
        assert!(r.findings.iter().any(|f| f.code == "incomplete_w2_inventory"));
        assert!(!r.findings.iter().any(|f| f.code == "se_owner_w2_mismatch"));
    }
    #[test]
    fn missing_intermediates_are_reported_without_zero_cascades() {
        let mut forms = business();
        let sc = forms.iter_mut().find(|f| f.kind == "f1040sc").unwrap();
        set(sc, "3. Subtract line 2", "");
        set(sc, "5. Gross profit", "");
        let r = report(&forms, &[employee()], true);
        assert_eq!(r.findings.iter().filter(|f| f.code == "missing_amount").count(), 2);
        assert!(!r.findings.iter().any(|f| f.line == "7. Gross income"));
    }
    #[test]
    fn source_wages_and_withholding_are_checked_independently() {
        let r = report(&[main_return(179000, 20000, 29900)], &[employee()], true);
        assert!(r.findings.iter().any(|f| f.code == "source_w2_wages_mismatch"));
        assert!(r.findings.iter().any(|f| f.code == "source_w2_withholding_mismatch"));
    }
    #[test]
    fn missing_status_and_identifiers_block_even_with_a_balanced_refund() {
        let mut f = main_return(0, 0, 0);
        for field in &mut f.fields {
            if matches!(field.kind, FieldKind::Checkbox { .. }) {
                field.value = None;
            }
        }
        set(&mut f, "Your social security number.", "");
        set(&mut f, "Spouse's social security number.", "");
        let r = report(&[f], &[], true);
        assert!(r.findings.iter().any(|f| f.code == "filing_status_count"));
        assert_eq!(r.findings.iter().filter(|f| f.code == "missing_identity").count(), 2);
    }
    #[test]
    fn two_filing_statuses_and_dependent_duplicate_ids_are_blocked() {
        let mut f = main_return(0, 0, 0);
        check_box(&mut f, "Head of household");
        for (s, v) in [
            ("(1) First name. Column: Dependent 1.", "Child"),
            ("(2) Last name. Column: Dependent 1.", "Household"),
            ("(3) S S N. Column: Dependent 1.", "000111111"),
            ("(4) Relationship. Column: Dependent 1.", "Daughter"),
        ] {
            set(&mut f, s, v);
        }
        let r = report(&[f], &[], true);
        assert!(r.findings.iter().any(|f| f.code == "filing_status_count"));
        assert!(r.findings.iter().any(|f| f.code == "duplicate_identifier"));
    }
    #[test]
    fn penalty_can_reduce_refund_or_create_debt_despite_an_overpayment() {
        let mut f = main_return(100000, 1000, 1500);
        set(&mut f, "38. Estimated tax penalty", "100");
        set(&mut f, "35a. Amount.", "400");
        assert_clean(&report(&[f], &[], false));
        let mut f = main_return(100000, 1000, 1500);
        set(&mut f, "38. Estimated tax penalty", "600");
        set(&mut f, "35a. Amount.", "0");
        set(&mut f, "Amount You Owe. 37.", "100");
        assert_clean(&report(&[f], &[], false));
        let mut f = main_return(100000, 1000, 500);
        set(&mut f, "38. Estimated tax penalty", "100");
        set(&mut f, "Amount You Owe. 37.", "600");
        assert_clean(&report(&[f], &[], false));
    }
    #[test]
    fn refund_allocation_and_negative_payment_lines_are_checked() {
        let mut f = main_return(100000, 1000, 1500);
        set(&mut f, "35a. Amount.", "400");
        assert!(
            report(&[f], &[], false)
                .findings
                .iter()
                .any(|i| i.code == "refund_allocation_mismatch")
        );
        let mut f = main_return(0, 0, 0);
        set(&mut f, "Amount You Owe. 37.", "-0.50");
        assert!(
            report(&[f], &[], true)
                .findings
                .iter()
                .any(|i| i.code == "negative_payment_or_refund")
        );
    }
    #[test]
    fn fiscal_year_dates_request_review_instead_of_using_calendar_rules() {
        let mut f = main_return(0, 0, 0);
        set(&mut f, "tax year beginning", "07/01");
        let r = report(&[f], &[], true);
        assert_eq!(r.status, "needs_review");
        assert_eq!(r.checks_run, 0);
    }
    #[test]
    fn malformed_money_cannot_turn_into_a_passing_zero() {
        let mut f = main_return(0, 0, 0);
        set(&mut f, "2b. Taxable interest", "NaN");
        let r = report(&[f], &[], true);
        assert_eq!(r.status, "blocked");
        assert!(r.findings.iter().any(|f| f.code == "invalid_amount"));
    }
    #[test]
    fn decimal_parser_and_rounding_do_not_use_binary_floats() {
        for (s, want) in [("$1,234.56", 123456), ("(45.67)", -4567), ("-0-", 0), ("1.5", 150)] {
            assert_eq!(money(s), Some(want));
        }
        for s in [
            "NaN",
            "inf",
            "1e3",
            "--1",
            "1.234",
            "",
            "9999999999999999999999999999999999999999",
        ] {
            assert_eq!(money(s), None);
        }
        assert_eq!(rate(452100, 1, 2), 226100);
        assert_eq!(rate(2955200, 29, 1000), 85700);
        assert!(label_matches("4. Amount.", "4. Amount."));
        assert!(!label_matches("14. Amount.", "4. Amount."));
    }
    #[test]
    fn nonzero_line_requires_its_supporting_schedule() {
        let mut f = main_return(0, 0, 0);
        set(&mut f, "23. Other taxes", "100");
        let r = report(&[f], &[], true);
        assert!(r.findings.iter().any(|f| f.code == "missing_supporting_form"));
    }
    #[test]
    fn duplicate_se_owner_is_detected_without_rejecting_two_spouses() {
        let mut forms = business();
        let mut duplicate = form("f1040sse", "-other");
        header(&mut duplicate, "000222222");
        forms.push(duplicate);
        assert!(
            report(&forms, &[employee()], true)
                .findings
                .iter()
                .any(|f| f.code == "duplicate_se_owner")
        );
        let last = forms.last_mut().unwrap();
        header(last, "000111111");
        assert!(
            !report(&forms, &[employee()], true)
                .findings
                .iter()
                .any(|f| f.code == "duplicate_se_owner")
        );
    }
    #[test]
    fn labelled_w2_transcription_has_a_real_source_not_model_supplied_amounts() {
        let dir = crate::testutil::scratch("validate-w2");
        let path = dir.join("statement.txt");
        std::fs::write(&path,"W-2 transcription, tax year 2025\nEmployee: Test, SSN 000-11-1111\nBox 1 wages: 180000.00\nBox 2 federal income tax withheld: 30000.00\nBox 3 Social security wages: 175000.00\nBox 7 Social security tips: 1100.00\n").unwrap();
        let w = read_w2(&path).unwrap();
        assert_eq!(w.social, 17_610_000);
        assert_eq!(w.owner, "000111111");
        std::fs::write(&path, "Employee: Test, SSN 000-11-1111\nBox 1 wages: NaN\n").unwrap();
        assert!(read_w2(&path).is_err());
    }
    #[test]
    fn rental_and_child_credit_return_passes_the_supported_rules() {
        let mut forms = business();
        let m = &mut forms[0];
        for (s, v) in [
            ("8. Additional income", 19000),
            ("9. Add lines 1z", 199000),
            ("11a. Subtract line 10", 198293),
            ("11b. Amount from line 11a", 198293),
            ("13a. Qualified business", 3659),
            ("14. Add lines", 35159),
            ("15. Subtract line 14", 163134),
            ("19. Child tax credit", 2200),
            ("21. Add lines", 2200),
            ("22. Subtract line 21", 17800),
            ("24. Add lines", 19213),
            ("Refund. 34.", 10787),
            ("35a. Amount.", 10787),
        ] {
            set(m, s, &v.to_string());
        }
        for (s, v) in [
            ("(1) First name. Column: Dependent 1.", "Child"),
            ("(2) Last name. Column: Dependent 1.", "Household"),
            ("(3) S S N. Column: Dependent 1.", "000333333"),
            ("(4) Relationship. Column: Dependent 1.", "Son"),
        ] {
            set(m, s, v);
        }
        check_box(m, "Row: (7) Credits. Column: Dependent 1. Child tax credit.");
        let s1 = forms.iter_mut().find(|f| f.kind == "f1040s1").unwrap();
        set(s1, "5. Rental real estate", "9000");
        set(s1, "10. Combine lines", "19000");
        let q = forms.iter_mut().find(|f| f.kind == "f8995").unwrap();
        for (s, v) in [
            ("Row: i i. Column: (c)", 9000),
            ("2. Total qualified business", 18293),
            ("4. Total qualified business", 18293),
            ("5. Qualified business income component", 3659),
            ("10. Qualified business income deduction before", 3659),
            ("11. Taxable income before", 166793),
            ("13. Subtract line 12", 166793),
            ("14. Income limitation", 33359),
            ("15. Qualified business income deduction", 3659),
        ] {
            set(q, s, &v.to_string());
        }
        let mut e = form("f1040se", "");
        header(&mut e, "000111111");
        for (row, value) in [
            (3, 12500),
            (9, 500),
            (12, 1000),
            (16, 1000),
            (18, 1000),
            (20, 3500),
            (21, 9000),
        ] {
            set(
                &mut e,
                &format!("Row: {row}.||Column: Properties. A."),
                &value.to_string(),
            );
        }
        for (s, v) in [
            ("23a. Total", 12500),
            ("23c. Total", 1000),
            ("23d. Total", 1000),
            ("23e. Total", 3500),
            ("24. Income.", 9000),
            ("26. Total rental", 9000),
        ] {
            set(&mut e, s, &v.to_string());
        }
        let mut s8 = form("f1040s8", "");
        header(&mut s8, "000111111");
        for (s, v) in [
            ("1. Enter the amount from line 11a", 198293),
            ("3. Add lines", 198293),
            ("4. Number of qualifying children", 1),
            ("5. Multiply line 4", 2200),
            ("8. Add lines", 2200),
            ("14. Enter the smaller", 2200),
        ] {
            set(&mut s8, s, &v.to_string());
        }
        forms.extend([e, s8]);
        assert_clean(&report(&forms, &[employee()], true));
    }
    #[test]
    fn valid_identifier_exceptions_are_reviewed_instead_of_marked_invalid() {
        let mut f = main_return(0, 0, 0);
        set(&mut f, "Spouse's social security number.", "NRA");
        let r = report(&[f], &[], true);
        assert!(r.findings.iter().any(|i| i.code == "identifier_exception"));
        assert!(!r.findings.iter().any(|i| i.code == "invalid_identifier_format"));
    }
    #[test]
    fn old_schedule_se_layout_is_not_checked_with_new_wage_base_rules() {
        let mut forms = business();
        let se = forms.iter_mut().find(|f| f.kind == "f1040sse").unwrap();
        for field in &mut se.fields {
            if let Some(d) = &mut field.description {
                *d = d.replace("for 2025", "for 2024");
            }
        }
        let r = report(&forms, &[employee()], true);
        assert!(r.findings.iter().any(|i| i.code == "unverified_form_year"));
    }
    #[test]
    fn fillable_w2_duplicate_copies_are_one_source_but_conflicts_are_unreadable() {
        let make = |label: &str, value: &str| Field {
            name: label.into(),
            description: Some(label.into()),
            page: Some(1),
            value: Some(value.into()),
            kind: FieldKind::Text {
                max_len: None,
                multiline: false,
            },
        };
        let mut fields = vec![
            make("a. Employee's social security number", "000111111"),
            make("b. Employer identification number", "00-3333333"),
            make("For calendar year", "2025"),
            make("1. Wages, tips, other compensation", "180000"),
            make("2. Federal income tax withheld", "30000"),
            make("3. Social security wages", "176100"),
            make("7. Social security tips", "0"),
        ];
        fields.extend(fields.clone());
        assert_eq!(w2_from_fields(&fields).unwrap().social, 17_610_000);
        fields.push(make("a. Employee's social security number", "000222222"));
        assert!(w2_from_fields(&fields).is_err());
    }
    #[test]
    fn source_period_must_match_the_return() {
        let dir = crate::testutil::scratch("validate-source-period");
        let path = dir.join("w2.txt");
        std::fs::write(&path,"Tax year 2024\nEmployee: Test, SSN 000-11-1111\nBox 1 wages: 180000\nBox 2 federal income tax withheld: 30000\nBox 3 Social security wages: 176100\n").unwrap();
        assert!(read_w2(&path).is_err());
    }
    #[test]
    fn missing_adult_identifier_does_not_imply_a_wrong_business_owner() {
        let mut forms = business();
        set(&mut forms[0], "Spouse's social security number.", "");
        let r = report(&forms, &[employee()], true);
        assert!(r.findings.iter().any(|i| i.code == "missing_identity"));
        assert!(!r.findings.iter().any(|i| i.code == "owner_not_on_return"));
    }
    #[test]
    fn unsupported_year_and_outside_source_are_not_silently_validated() {
        let ws = crate::testutil::scratch("validate-paths");
        std::fs::create_dir(ws.join("forms")).unwrap();
        let r = run(&ws, 2024, Some("forms"), &[], true).unwrap();
        assert_eq!(r.status, "needs_review");
        assert_eq!(r.checks_run, 0);
        assert!(run(&ws, 2025, Some("forms"), &["/etc/passwd".into()], true).is_err());
        assert!(run(&ws, 2025, Some("/etc"), &[], true).is_err());
    }
}
