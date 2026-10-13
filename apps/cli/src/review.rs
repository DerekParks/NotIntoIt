//! Final review artifacts derive from current PDFs and server-recorded source access.
//! A sourced disagreement never waives a deterministic finding.
use crate::{documents, instructions, validate};
use anyhow::{Context, Result, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct Dispute {
    /// Finding code from the latest validation report.
    pub code: String,
    /// Exact form path shown in that finding.
    pub form: String,
    /// Exact field name, if present in the finding.
    pub field: Option<String>,
    /// Explain the disagreement. It remains pending manual review.
    pub rationale: String,
    /// IRS instruction PDF path returned by get_form_instructions in this session.
    pub source_pdf: String,
    /// 1-based page previously returned by get_form_instructions.
    pub page: u32,
    /// Exact supporting excerpt (10-2000 characters) from that returned page.
    pub quote: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Snapshot {
    pub path: String,
    pub purpose: &'static str,
    pub sha256: String,
    pub editable_fields: Option<usize>,
    pub populated_fields: Option<usize>,
    pub included_in_validator: bool,
}

#[derive(Debug, Serialize)]
pub struct SourceLink {
    pub finding_index: usize,
    pub document_stem: String,
    pub url: String,
    pub access_status: &'static str,
    pub pages_returned: Vec<u32>,
    pub lookup_hint: String,
}

#[derive(Debug, Serialize)]
pub struct ReviewedDispute {
    pub code: String,
    pub form: String,
    pub field: Option<String>,
    pub rationale: String,
    pub source_pdf: String,
    pub page: u32,
    pub quote: String,
    pub source_url: Option<String>,
    pub source_access_recorded: bool,
    pub status: &'static str,
    pub explanation: String,
}

#[derive(Debug, Serialize)]
pub struct ReviewReport {
    pub tax_year: u16,
    pub status: &'static str,
    pub allowed_final_statement: String,
    pub validation: validate::Report,
    pub snapshots: Vec<Snapshot>,
    pub source_links: Vec<SourceLink>,
    pub instruction_access: Vec<instructions::ReadEvidence>,
    pub disputes: Vec<ReviewedDispute>,
    pub json_path: String,
    pub readable_path: String,
    pub limits: &'static str,
}

pub fn create(
    workspace: &Path,
    year: u16,
    folder: Option<&str>,
    w2s: &[String],
    complete: bool,
    reads: &[instructions::ReadEvidence],
    disputes: &[Dispute],
) -> Result<ReviewReport> {
    ensure!((1900..=2100).contains(&year), "tax year must be between 1900 and 2100");
    let root = workspace.canonicalize()?;
    let folder = folder.map(str::to_owned).unwrap_or_else(|| format!("{year}/forms"));
    let before = snapshot(&root, &folder, w2s)?;
    let validation = validate::run(&root, year, Some(&folder), w2s, complete)?;
    let mut snapshots = snapshot(&root, &folder, w2s)?;
    ensure!(
        before == snapshots,
        "Return PDFs or W-2 sources changed during review; rerun create_review_report"
    );
    for snapshot in &mut snapshots {
        snapshot.included_in_validator = validation.forms_checked.contains(&snapshot.path)
            || validation.w2_sources_checked.iter().any(|p| {
                documents::contained(&root, Path::new(p)).ok()
                    == documents::contained(&root, Path::new(&snapshot.path)).ok()
            });
    }
    let valid_reads = reads
        .iter()
        .filter(|r| r.tax_year == year && current_read(&root, r))
        .cloned()
        .collect::<Vec<_>>();
    let source_links = validation.findings.iter().enumerate().filter_map(|(i, finding)| {
        let form = finding_form(finding)?;
        let (stem, kind) = instructions::mapping(&form).ok()?;
        let accessed: Vec<_> = valid_reads.iter().filter(|r| r.document_stem == stem).collect();
        let mut pages = accessed.iter().flat_map(|r| r.pages_returned.iter().copied()).collect::<Vec<_>>();
        pages.sort_unstable(); pages.dedup();
        Some(SourceLink { finding_index: i, url: nii_pdf::irs::form_url(&stem, year), document_stem: stem,
            access_status: if pages.is_empty() { "not_read_in_session" } else { "text_returned_in_session" },
            pages_returned: pages,
            lookup_hint: if finding.code == "se_owner_w2_mismatch" {
                "Read Schedule SE instructions: search Joint Returns; each spouse's SE schedule is individual. Compare with that owner's W-2 source documents.".into()
            } else { format!("{kind}; inspect the instructions for {}. A link alone is not retrieved evidence.", finding.line) },
        })
    }).collect();
    let mut reviewed_disputes = Vec::new();
    for dispute in disputes {
        ensure!(
            !dispute.rationale.trim().is_empty() && dispute.rationale.chars().count() <= 4000,
            "dispute rationale must contain 1-4000 characters"
        );
        ensure!(
            validation
                .findings
                .iter()
                .any(|f| f.code == dispute.code && f.form == dispute.form && f.field == dispute.field),
            "dispute must identify a current finding exactly (code, form, field); rerun validation after corrections"
        );
        let accessed = verified_quote(&root, year, &valid_reads, dispute);
        reviewed_disputes.push(ReviewedDispute {
            code: dispute.code.clone(), form: dispute.form.clone(), field: dispute.field.clone(),
            rationale: dispute.rationale.clone(), source_pdf: dispute.source_pdf.clone(), page: dispute.page,
            quote: dispute.quote.clone(), source_url: accessed.map(|r| r.url.clone()),
            source_access_recorded: accessed.is_some(),
            status: if accessed.is_some() { "pending_manual_review" } else { "source_required" },
            explanation: if accessed.is_some() {
                "The quoted text was returned by get_form_instructions for this tax year and its PDF is unchanged. Relevance and interpretation are not verified. The finding remains unresolved; no checks were waived."
            } else {
                "Read the relevant IRS instructions with get_form_instructions for this tax year, then cite a delivered page and exact excerpt. A supplied URL, unseen page, changed PDF, empty page, or invented quote does not establish source access. The finding remains unresolved."
            }.into(),
        });
    }
    let status = if validation.blocking_errors > 0 {
        "blocked"
    } else if validation.review_items > 0 || !reviewed_disputes.is_empty() {
        "needs_review"
    } else {
        "checks_passed"
    };
    let statement = match status {
        "blocked" => format!(
            "Return review is blocked: {} deterministic error(s), {} manual review item(s). Do not describe the return as complete or the unresolved checks as passed.",
            validation.blocking_errors, validation.review_items
        ),
        "needs_review" => format!(
            "No blocking deterministic errors were reported, but manual review is required: {} validator review item(s), {} unresolved dispute(s). Do not claim all checks passed or filing readiness.",
            validation.review_items,
            reviewed_disputes.len()
        ),
        _ => format!(
            "The {} listed deterministic checks passed for the captured PDFs. Eligibility, source completeness beyond supplied W-2s, form necessity, unimplemented lines and filing readiness are not certified.",
            validation.checks_run
        ),
    };
    let json_path = format!("{year}/review/return-review.json");
    let readable_path = format!("{year}/review/return-review.md");
    let report = ReviewReport {
        tax_year: year,
        status,
        allowed_final_statement: statement,
        validation,
        snapshots,
        source_links,
        instruction_access: valid_reads,
        disputes: reviewed_disputes,
        json_path,
        readable_path,
        limits: "This report captures current PDFs/W-2s by SHA-256 and reruns bounded validation. Any later edit invalidates it; regenerate after all edits. Instruction access records text delivered by this MCP session, not human review, legal applicability, or proof that a disputed rule is wrong. Disputes never override findings. Source links may be unavailable exact-year candidates. No signatures or e-filing; the user must review the return.",
    };
    let json = output_path(&root, &report.json_path)?;
    let readable = output_path(&root, &report.readable_path)?;
    // Both destinations must pass containment before either artifact is written.
    std::fs::write(json, serde_json::to_vec_pretty(&report)?)?;
    std::fs::write(readable, render(&report))?;
    Ok(report)
}

fn current_read(root: &Path, read: &instructions::ReadEvidence) -> bool {
    documents::contained(root, Path::new(&read.pdf))
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .is_some_and(|bytes| instructions::sha256(&bytes) == read.sha256)
}

fn verified_quote<'a>(
    root: &Path,
    year: u16,
    reads: &'a [instructions::ReadEvidence],
    d: &Dispute,
) -> Option<&'a instructions::ReadEvidence> {
    if d.page == 0 || !(10..=2000).contains(&d.quote.chars().count()) {
        return None;
    }
    let path = documents::contained(root, Path::new(&d.source_pdf)).ok()?;
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let quote = norm(&d.quote);
    if quote.chars().count() < 10 {
        return None;
    }
    reads.iter().find(|r| {
        r.tax_year == year
            && current_read(root, r)
            && documents::contained(root, Path::new(&r.pdf)).ok().as_ref() == Some(&path)
            && r.delivered_text
                .iter()
                .any(|(p, text)| *p == d.page && norm(text).contains(&quote))
    })
}

fn finding_form(f: &validate::Finding) -> Option<String> {
    if f.code == "incomplete_w2_inventory" {
        return Some("Schedule SE".into());
    }
    if f.code.starts_with("se_") {
        return Some("Schedule SE".into());
    }
    if f.code.contains("w2") || f.line == "source" {
        return Some("W-2".into());
    }
    let name = Path::new(&f.form).file_stem()?.to_str()?;
    if !name.starts_with('f') {
        return None;
    }
    Some(name.split(['-', '_']).next()?.to_string())
}

fn snapshot(root: &Path, folder: &str, w2s: &[String]) -> Result<Vec<Snapshot>> {
    let dir = documents::contained(root, Path::new(folder))?;
    let mut paths = std::fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.retain(|p| p.extension().is_some_and(|s| s.eq_ignore_ascii_case("pdf")));
    let mut selected = paths.into_iter().map(|p| (p, "return_pdf")).collect::<Vec<_>>();
    for source in w2s {
        selected.push((documents::contained(root, Path::new(source))?, "w2_source"));
    }
    let mut out = Vec::new();
    for (path, purpose) in selected {
        let path = documents::contained(root, &path)?;
        let bytes = std::fs::read(&path)?;
        let fields = if purpose == "return_pdf" {
            nii_pdf::acroform::normalize(&bytes)
                .and_then(|b| nii_pdf::acroform::read_fields(&b))
                .ok()
        } else {
            None
        };
        out.push(Snapshot {
            path: path.strip_prefix(root)?.display().to_string(),
            purpose,
            sha256: instructions::sha256(&bytes),
            editable_fields: fields.as_ref().map(Vec::len),
            populated_fields: fields.as_ref().map(|fs| {
                fs.iter()
                    .filter(|f| f.value.as_deref().is_some_and(|v| !v.trim().is_empty() && v != "Off"))
                    .count()
            }),
            included_in_validator: false,
        });
    }
    out.sort_by(|a, b| (&a.path, a.purpose).cmp(&(&b.path, b.purpose)));
    out.dedup();
    Ok(out)
}

fn output_path(root: &Path, relative: &str) -> Result<std::path::PathBuf> {
    let full = root.join(relative);
    let mut ancestor = full.parent().context("review output has no parent")?;
    while ancestor.symlink_metadata().is_err() {
        ancestor = ancestor.parent().context("review output has no ancestor")?;
    }
    documents::contained(root, ancestor)?;
    std::fs::create_dir_all(full.parent().unwrap())?;
    documents::contained_new(root, &full)
}

fn render(r: &ReviewReport) -> String {
    let mut out = format!(
        "# Return review — {}\n\nStatus: **{}**\n\n{}\n\n## Captured files\n\n",
        r.tax_year, r.status, r.allowed_final_statement
    );
    for f in &r.snapshots {
        out.push_str(&format!(
            "- `{}` ({}; populated fields: {:?}; included in validator: {}; SHA-256 `{}`)\n",
            f.path, f.purpose, f.populated_fields, f.included_in_validator, f.sha256
        ));
    }
    out.push_str("\n## Unresolved findings\n\n");
    if r.validation.findings.is_empty() {
        out.push_str("No findings from the listed deterministic checks.\n");
    }
    for (i, f) in r.validation.findings.iter().enumerate() {
        out.push_str(&format!(
            "- **{} / {}** — `{}`, {}{}: {}\n",
            f.severity,
            f.code,
            f.form,
            f.line,
            f.field.as_ref().map(|v| format!(" (`{v}`)")).unwrap_or_default(),
            f.message
        ));
        if let Some(s) = r.source_links.iter().find(|s| s.finding_index == i) {
            out.push_str(&format!(
                "  Source: [IRS {}]({}); access: {}; returned pages: {:?}. {}\n",
                s.document_stem, s.url, s.access_status, s.pages_returned, s.lookup_hint
            ));
        }
    }
    out.push_str("\n## Instruction text returned in this session\n\n");
    if r.instruction_access.is_empty() {
        out.push_str("No current, same-year instruction access recorded. IRS lookup links above do not establish source access.\n");
    }
    for source in &r.instruction_access {
        out.push_str(&format!(
            "- [IRS {}]({}) — tax year {}; `{}`; returned pages: {:?}; SHA-256 `{}`. Text delivery does not verify applicability or interpretation.\n",
            source.document_stem, source.url, source.tax_year, source.pdf, source.pages_returned, source.sha256
        ));
    }
    out.push_str("\n## Disputes requiring manual review\n\n");
    if r.disputes.is_empty() {
        out.push_str("No disputes recorded.\n");
    }
    for d in &r.disputes {
        out.push_str(&format!("- **{}** — {} / `{}`: {}\n  Source: `{}`, page {}; source access recorded: {}.\n  Claimed excerpt: {}\n  {}\n", d.status, d.code, d.form, d.rationale, d.source_pdf, d.page, d.source_access_recorded, d.quote, d.explanation));
    }
    out.push_str(&format!(
        "\n## Coverage limits\n\n{}\n\n{}\n",
        r.validation.limits, r.limits
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn workspace(label: &str) -> std::path::PathBuf {
        let ws = crate::testutil::scratch(label);
        std::fs::create_dir_all(ws.join("2025/forms")).unwrap();
        std::fs::write(ws.join("2025/forms/f1040.pdf"), crate::testutil::F1040).unwrap();
        ws
    }
    fn evidence(ws: &Path) -> instructions::ReadEvidence {
        std::fs::create_dir_all(ws.join("2025/instructions")).unwrap();
        let bytes = b"%PDF fixture bytes";
        std::fs::write(ws.join("2025/instructions/i1040sse--2025.pdf"), bytes).unwrap();
        instructions::ReadEvidence {
            tax_year: 2025,
            document_stem: "i1040sse".into(),
            url: nii_pdf::irs::form_url("i1040sse", 2025),
            pdf: "2025/instructions/i1040sse--2025.pdf".into(),
            sha256: instructions::sha256(bytes),
            pages_returned: vec![2],
            delivered_text: vec![(2, "Each spouse must file a separate Schedule SE.".into())],
        }
    }
    fn dispute() -> Dispute {
        Dispute {
            code: "se_owner_w2_mismatch".into(),
            form: "2025/forms/f1040sse.pdf".into(),
            field: None,
            rationale: "Please review the owner allocation.".into(),
            source_pdf: "2025/instructions/i1040sse--2025.pdf".into(),
            page: 2,
            quote: "Each spouse must file a separate Schedule SE.".into(),
        }
    }
    #[test]
    fn source_evidence_requires_delivered_page_exact_quote_year_and_unchanged_pdf() {
        let ws = workspace("review-source");
        let evidence = evidence(&ws);
        let mut d = dispute();
        assert!(verified_quote(&ws, 2025, std::slice::from_ref(&evidence), &d).is_some());
        d.page = 1;
        assert!(verified_quote(&ws, 2025, std::slice::from_ref(&evidence), &d).is_none());
        d.page = 2;
        d.quote = "Combine both spouses' wages.".into();
        assert!(verified_quote(&ws, 2025, std::slice::from_ref(&evidence), &d).is_none());
        d = dispute();
        assert!(verified_quote(&ws, 2024, std::slice::from_ref(&evidence), &d).is_none());
        assert!(verified_quote(&ws, 2025, &[], &d).is_none());
        std::fs::write(ws.join(&evidence.pdf), b"changed").unwrap();
        assert!(verified_quote(&ws, 2025, &[evidence], &d).is_none());
    }
    #[test]
    fn review_reruns_validator_saves_both_artifacts_and_never_waives_disputes() {
        let ws = workspace("review-report");
        let first = create(&ws, 2025, None, &[], false, &[], &[]).unwrap();
        assert_eq!(first.status, "blocked");
        assert!(first.validation.blocking_errors > 0);
        assert_eq!(first.snapshots[0].sha256, instructions::sha256(crate::testutil::F1040));
        assert!(first.snapshots[0].included_in_validator);
        assert!(ws.join(&first.json_path).exists() && ws.join(&first.readable_path).exists());
        assert!(
            first
                .source_links
                .iter()
                .all(|s| s.access_status == "not_read_in_session")
        );
        let finding = &first.validation.findings[0];
        let mut d = dispute();
        d.code = finding.code.into();
        d.form = finding.form.clone();
        d.field = finding.field.clone();
        let evidence = evidence(&ws);
        let report = create(
            &ws,
            2025,
            None,
            &[],
            false,
            std::slice::from_ref(&evidence),
            &[d.clone()],
        )
        .unwrap();
        assert_eq!(report.status, "blocked");
        assert_eq!(report.validation.blocking_errors, first.validation.blocking_errors);
        assert_eq!(report.disputes[0].status, "pending_manual_review");
        assert!(report.disputes[0].source_access_recorded);
        let readable = std::fs::read_to_string(ws.join(&report.readable_path)).unwrap();
        assert!(readable.contains("## Instruction text returned in this session"));
        assert!(readable.contains(&evidence.sha256));
        assert!(readable.contains("returned pages: [2]"));
        assert!(
            report
                .allowed_final_statement
                .contains("Do not describe the return as complete")
        );
        let report = create(&ws, 2025, None, &[], false, &[], &[d]).unwrap();
        assert_eq!(report.disputes[0].status, "source_required");
        assert!(!report.disputes[0].source_access_recorded);
        assert_eq!(report.validation.blocking_errors, first.validation.blocking_errors);
    }
    #[test]
    fn unsupported_year_has_no_pass_statement_and_report_refreshes_hashes() {
        let ws = workspace("review-year");
        let before = create(&ws, 2024, Some("2025/forms"), &[], false, &[], &[]).unwrap();
        assert_eq!(before.status, "needs_review");
        assert_eq!(before.validation.checks_run, 0);
        assert!(before.allowed_final_statement.contains("manual review is required"));
        std::fs::write(ws.join("2025/forms/f1040.pdf"), b"corrupt now").unwrap();
        let after = create(&ws, 2025, None, &[], false, &[], &[]).unwrap();
        assert_ne!(before.snapshots[0].sha256, after.snapshots[0].sha256);
        assert_eq!(after.status, "blocked");
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(ws.join(after.json_path)).unwrap()).unwrap();
        assert_eq!(saved["snapshots"][0]["sha256"], after.snapshots[0].sha256);
    }
    #[test]
    fn sources_are_bound_to_the_correct_form_and_dispute_must_match_current_finding() {
        let f = validate::Finding {
            severity: "error",
            code: "se_owner_w2_mismatch",
            form: "2025/forms/f1040sse.pdf".into(),
            field: None,
            line: "8a".into(),
            message: "owner mismatch".into(),
        };
        assert_eq!(finding_form(&f).unwrap(), "Schedule SE");
        let f = validate::Finding {
            code: "missing_amount",
            form: "2025/forms/f1040sc-second.pdf".into(),
            ..f
        };
        assert_eq!(finding_form(&f).unwrap(), "f1040sc");
        let ws = workspace("review-dispute-target");
        assert!(
            create(&ws, 2025, None, &[], false, &[], &[dispute()])
                .unwrap_err()
                .to_string()
                .contains("current finding")
        );
    }
    #[cfg(unix)]
    #[test]
    fn review_outputs_and_source_access_refuse_external_or_dangling_symlinks() {
        use std::os::unix::fs::symlink;
        let ws = workspace("review-escape");
        let outside = crate::testutil::scratch("review-outside");
        symlink(&outside, ws.join("2025/review")).unwrap();
        assert!(
            create(&ws, 2025, None, &[], false, &[], &[])
                .unwrap_err()
                .to_string()
                .contains("outside the tax workspace")
        );
        assert!(!outside.join("return-review.json").exists());
        let ws = workspace("review-file-escape");
        std::fs::create_dir_all(ws.join("2025/review")).unwrap();
        std::fs::write(outside.join("private"), b"unchanged").unwrap();
        symlink(outside.join("private"), ws.join("2025/review/return-review.md")).unwrap();
        assert!(create(&ws, 2025, None, &[], false, &[], &[]).is_err());
        assert!(!ws.join("2025/review/return-review.json").exists());
        assert_eq!(std::fs::read(outside.join("private")).unwrap(), b"unchanged");
        let mut e = evidence(&ws);
        e.pdf = "/etc/hostname".into();
        assert!(!current_read(&ws, &e));
    }
}
