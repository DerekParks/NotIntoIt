mod documents;
mod forms;
mod index;
mod instructions;
mod mcp;
mod review;
#[cfg(test)]
mod testutil;
mod validate;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use nii_core::SourceKind;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "nii", about = "NotIntoIt — LLM-assisted tax prep", version)]
struct Cli {
    /// Data directory: corpus and form caches, models, pdfium, index
    /// (default: the platform data dir, e.g. ~/.local/share/notintoit).
    #[arg(long, global = true, env = "NII_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// Index database path (default: <data-dir>/index.db).
    #[arg(long, global = true, env = "NII_DB")]
    db: Option<PathBuf>,
    /// Tax workspace holding per-year forms (default: ~/Taxes).
    #[arg(long, global = true, env = "NII_WORKSPACE")]
    workspace: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Corpus operations.
    Corpus {
        #[command(subcommand)]
        command: CorpusCommand,
    },
    /// Hybrid search over the local index.
    Search {
        query: Vec<String>,
        /// Max results.
        #[arg(short = 'k', long, default_value_t = 8)]
        limit: usize,
        /// Restrict to one source (usc, cfr, irs-pub, irs-instr).
        #[arg(long)]
        source: Option<String>,
    },
    /// IRS PDF forms: fetch, inspect, fill, view.
    Form {
        #[command(subcommand)]
        command: FormCommand,
    },
    /// Run the MCP server on stdio (builds the search index in the background).
    Mcp,
    /// Save a sourced final review report and fingerprints of current return PDFs.
    Review {
        #[arg(long)]
        year: u16,
        #[arg(long)]
        forms_dir: Option<String>,
        #[arg(long = "w2")]
        w2_documents: Vec<String>,
        #[arg(long)]
        w2_complete: bool,
    },
    /// Check filled 2025 federal forms for missing fields, arithmetic, and transfers. Does not edit forms.
    Validate {
        #[arg(long)]
        year: u16,
        /// Workspace-relative directory (default: <year>/forms).
        #[arg(long)]
        forms_dir: Option<String>,
        /// Workspace-relative W-2 source path; repeat for each W-2. Supports fillable PDFs and labelled text.
        #[arg(long = "w2")]
        w2_documents: Vec<String>,
        /// All W-2s for this return are supplied; permits checking zero wages for a spouse without a W-2.
        #[arg(long)]
        w2_complete: bool,
    },
}

#[derive(Subcommand)]
enum FormCommand {
    /// Read exact-year IRS instructions (shared or embedded where mapped).
    Instructions {
        form: String,
        #[arg(long)]
        year: u16,
        #[arg(long)]
        page: Option<u32>,
        #[arg(long)]
        search: Option<String>,
    },
    /// Download a blank form into <workspace>/<year>/forms/ and print its path.
    Fetch {
        /// Form name or IRS file stem: 1040, W-2, f1040s1, ...
        form: String,
        #[arg(long)]
        year: u16,
        /// Replace an existing (possibly filled) working copy.
        #[arg(long)]
        overwrite: bool,
    },
    /// Print a form's fillable fields as JSON.
    Fields {
        pdf: PathBuf,
        #[arg(long)]
        page: Option<u32>,
    },
    /// Fill fields from a JSON object of {"field name": value}.
    Fill {
        pdf: PathBuf,
        /// JSON file of field values.
        #[arg(long)]
        values: PathBuf,
        /// Write here instead of updating PDF in place.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Write a flat copy with form values burned into the pages.
    Flatten {
        pdf: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Render a page to PNG, or open the PDF in the default viewer.
    View {
        pdf: PathBuf,
        #[arg(long, default_value_t = 1)]
        page: u32,
        /// Write the rendered page here instead of opening the viewer.
        #[arg(long)]
        png: Option<PathBuf>,
        #[arg(long, default_value_t = 1700)]
        width: u32,
    },
}

#[derive(Subcommand)]
enum CorpusCommand {
    /// Download, parse, chunk, embed, and index the corpus (replaces the index).
    Build {
        /// Only index the first N chunks (dev shortcut).
        #[arg(long)]
        limit_chunks: Option<usize>,
        /// Re-download sources even if cached.
        #[arg(long)]
        force_download: bool,
        /// Embedding batch size.
        #[arg(long, default_value_t = 64)]
        batch_size: usize,
        /// CUDA device id to embed on (requires a build with --features nii-embed/cuda).
        #[arg(long)]
        cuda_device: Option<i32>,
    },
    /// Print index provenance and counts.
    Stats,
    /// Download this version's prebuilt index from GitHub Releases (replaces the index).
    Fetch {
        /// Download from here instead (also NII_INDEX_URL).
        #[arg(long)]
        url: Option<String>,
    },
    /// Write a compressed copy of the index and its checksum for attaching to a release.
    Pack {
        #[arg(long, default_value = "target/index-release")]
        output: PathBuf,
    },
}

fn main() -> Result<()> {
    run(Cli::parse())
}

fn run(cli: Cli) -> Result<()> {
    let data_dir = match cli.data_dir {
        Some(d) => d,
        None => dirs::data_dir()
            .context("no platform data dir; pass --data-dir")?
            .join("notintoit"),
    };
    let db_path = cli.db.unwrap_or_else(|| data_dir.join("index.db"));
    let workspace = match cli.workspace {
        Some(w) => w,
        None => dirs::home_dir().context("no home dir; pass --workspace")?.join("Taxes"),
    };
    match cli.command {
        Command::Corpus {
            command:
                CorpusCommand::Build {
                    limit_chunks,
                    force_download,
                    batch_size,
                    cuda_device,
                },
        } => build(
            &data_dir,
            &db_path,
            &index::BuildOptions {
                limit_chunks,
                force_download,
                batch_size,
                cuda_device,
            },
        ),
        Command::Corpus {
            command: CorpusCommand::Stats,
        } => stats(&db_path),
        Command::Corpus {
            command: CorpusCommand::Fetch { url },
        } => fetch(&db_path, url),
        Command::Corpus {
            command: CorpusCommand::Pack { output },
        } => {
            let asset = index::pack(&db_path, &output)?;
            println!("{}", asset.display());
            Ok(())
        }
        Command::Search { query, limit, source } => {
            let source = match source.as_deref() {
                None => None,
                Some(s) => Some(
                    SourceKind::parse(s)
                        .with_context(|| format!("unknown source '{s}' (usc, cfr, irs-pub, irs-instr)"))?,
                ),
            };
            search(&data_dir, &db_path, &query.join(" "), limit, source)
        }
        Command::Form { command } => form(&data_dir, &workspace, command),
        Command::Review {
            year,
            forms_dir,
            w2_documents,
            w2_complete,
        } => {
            let report = review::create(
                &workspace,
                year,
                forms_dir.as_deref(),
                &w2_documents,
                w2_complete,
                &[],
                &[],
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if report.status != "checks_passed" {
                bail!("return review {}: see {}", report.status, report.json_path);
            }
            Ok(())
        }
        Command::Validate {
            year,
            forms_dir,
            w2_documents,
            w2_complete,
        } => {
            let report = validate::run(&workspace, year, forms_dir.as_deref(), &w2_documents, w2_complete)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if report.status != "checks_passed" {
                bail!(
                    "validation {}: {} blocking error(s), {} review item(s)",
                    report.status,
                    report.blocking_errors,
                    report.review_items
                );
            }
            Ok(())
        }
        Command::Mcp => mcp::serve(mcp::Paths {
            data_dir,
            db_path,
            workspace,
        }),
    }
}

fn fetch(db_path: &Path, url: Option<String>) -> Result<()> {
    let url = url
        .or_else(index::prebuilt_url)
        .context("NII_INDEX_URL is \"none\"; pass --url")?;
    let lock = index::try_lock(db_path)?.context("another process is building this index")?;
    let bar = ProgressBar::new(0).with_style(ProgressStyle::with_template("{bar:40} {bytes}/{total_bytes}").unwrap());
    let bytes = index::fetch(&url, db_path, &lock, &mut |p| {
        if let index::Progress::Fetching { bytes, total } = p {
            bar.set_length(total.unwrap_or(bytes));
            bar.set_position(bytes);
        }
    })?;
    bar.finish();
    println!("installed {bytes} bytes from {url} -> {}", db_path.display());
    Ok(())
}

fn build(data_dir: &Path, db_path: &Path, options: &index::BuildOptions) -> Result<()> {
    let lock = index::try_lock(db_path)?.context("another process is building this index")?;
    let bar = ProgressBar::new_spinner();
    let built = index::build(data_dir, db_path, &lock, options, &mut |p| match p {
        index::Progress::Fetching { .. } => {}
        index::Progress::Downloading => bar.set_message("downloading Title 26 and IRS guidance"),
        index::Progress::Parsing => bar.set_message("parsing"),
        index::Progress::Embedding { done, total } => {
            if bar.length() != Some(total as u64) {
                bar.set_length(total as u64);
                bar.set_style(ProgressStyle::with_template("{bar:40} {pos}/{len} chunks  eta {eta}").unwrap());
            }
            bar.set_position(done as u64);
        }
    })?;
    bar.finish();
    println!(
        "indexed {} chunks (Title 26 @ {}) in {:.0?} -> {}",
        built.chunks,
        built.release_point,
        built.elapsed,
        db_path.display()
    );
    Ok(())
}

fn form(data_dir: &Path, workspace: &Path, command: FormCommand) -> Result<()> {
    match command {
        FormCommand::Instructions {
            form,
            year,
            page,
            search,
        } => {
            println!(
                "{}",
                instructions::read(data_dir, workspace, &form, year, page, search.as_deref())?
            );
        }
        FormCommand::Fetch { form, year, overwrite } => {
            let fetched = forms::fetch(data_dir, workspace, &form, year, overwrite)?;
            if !fetched.created {
                eprintln!("kept existing working copy (--overwrite to replace)");
            }
            if let Some(revision) = fetched.revision.filter(|r| *r != year) {
                eprintln!("using the {revision} revision, in effect for {year}");
            }
            println!("{}", fetched.path.display());
        }
        FormCommand::Fields { pdf, page } => {
            let fields = nii_pdf::acroform::read_fields(&std::fs::read(&pdf)?)?;
            let fields: Vec<_> = fields
                .into_iter()
                .filter(|f| page.is_none_or(|p| f.page == Some(p)))
                .collect();
            println!("{}", serde_json::to_string_pretty(&fields)?);
        }
        FormCommand::Fill { pdf, values, output } => {
            let json: BTreeMap<String, serde_json::Value> = serde_json::from_slice(
                &std::fs::read(&values).with_context(|| format!("reading {}", values.display()))?,
            )?;
            let values = json
                .iter()
                .map(|(k, v)| Ok((k.clone(), forms::value_from_json(k, v)?)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            let (path, filled) = forms::fill(&pdf, &values, output.as_deref())?;
            if filled.pdf.is_some() {
                println!("{}", path.display());
            }
            if !filled.problems.is_empty() {
                let written = if filled.pdf.is_some() {
                    "the other fields were written"
                } else {
                    "nothing written"
                };
                bail!("{written}; fix these fields:\n  {}", filled.problems.join("\n  "));
            }
        }
        FormCommand::Flatten { pdf, output } => {
            let flat = nii_pdf::fill::flatten(nii_pdf::engine::pdfium(data_dir)?, &std::fs::read(&pdf)?)?;
            std::fs::write(&output, flat)?;
            println!("{}", output.display());
        }
        FormCommand::View { pdf, page, png, width } => match png {
            Some(out) => {
                std::fs::write(&out, forms::render(data_dir, &pdf, page, width)?.png)?;
                println!("{}", out.display());
            }
            None => open::that_detached(&pdf)?,
        },
    }
    Ok(())
}

fn stats(db_path: &Path) -> Result<()> {
    let conn = nii_search::open(db_path)?;
    for key in [
        "schema_version",
        "model",
        "dim",
        "usc_release_point",
        "guidance",
        "built_at_unix",
        "chunks",
    ] {
        println!(
            "{key}: {}",
            nii_search::get_meta(&conn, key)?.unwrap_or_else(|| "-".into())
        );
    }
    Ok(())
}

fn search(data_dir: &Path, db_path: &Path, query: &str, limit: usize, source: Option<SourceKind>) -> Result<()> {
    anyhow::ensure!(!query.trim().is_empty(), "empty query");
    let conn = nii_search::open(db_path).context("no index — run `nii corpus build` first")?;
    let mut embedder = nii_embed::Embedder::new(&data_dir.join("models"), nii_embed::Device::default())?;
    let query_embedding = embedder.embed_query(query)?;

    let hits = nii_search::search(&conn, query, &query_embedding, source, limit)?;
    if hits.is_empty() {
        println!("no results");
        return Ok(());
    }
    for (i, hit) in hits.iter().enumerate() {
        println!("{:2}. [{:.4}] {} — {}", i + 1, hit.score, hit.citation, hit.heading);
        if let Some(caveat) = &hit.caveat {
            println!("    ⚠ {caveat}");
        }
        if !hit.section_path.is_empty() {
            println!("    {}", hit.section_path);
        }
        println!("    {}", snippet(&hit.text, 220));
        if let Some(url) = &hit.url {
            println!("    {url}");
        }
    }
    Ok(())
}

fn snippet(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", text[..end].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[test]
    fn cli_parses_global_flags_and_subcommands() {
        let cli = Cli::try_parse_from([
            "nii",
            "--data-dir",
            "/d",
            "--workspace",
            "/w",
            "form",
            "view",
            "f.pdf",
            "--page",
            "2",
        ])
        .unwrap();
        assert_eq!(cli.data_dir.as_deref(), Some(Path::new("/d")));
        assert_eq!(cli.workspace.as_deref(), Some(Path::new("/w")));
        assert!(matches!(
            cli.command,
            Command::Form {
                command: FormCommand::View {
                    page: 2,
                    width: 1700,
                    png: None,
                    ..
                }
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["nii", "corpus", "build", "--limit-chunks", "5"])
                .unwrap()
                .command,
            Command::Corpus {
                command: CorpusCommand::Build {
                    limit_chunks: Some(5),
                    batch_size: 64,
                    ..
                }
            }
        ));
        assert!(
            Cli::try_parse_from(["nii", "form", "fetch", "1040"]).is_err(),
            "--year is required"
        );
    }

    #[test]
    fn form_commands_fetch_list_fill_flatten_and_render() {
        let data = testutil::data_dir();
        let ws = testutil::scratch("cli-form");
        let pdf = ws.join("2025/forms/f1040.pdf");
        form(
            &data,
            &ws,
            FormCommand::Fetch {
                form: "1040".into(),
                year: 2025,
                overwrite: false,
            },
        )
        .unwrap();
        assert!(pdf.exists());
        form(
            &data,
            &ws,
            FormCommand::Fetch {
                form: "1040".into(),
                year: 2025,
                overwrite: false,
            },
        )
        .unwrap();
        form(
            &data,
            &ws,
            FormCommand::Fields {
                pdf: pdf.clone(),
                page: Some(1),
            },
        )
        .unwrap();

        let values = ws.join("values.json");
        std::fs::write(&values, r#"{"f1_14[0]": "Jane", "f1_47[0]": 72805}"#).unwrap();
        form(
            &data,
            &ws,
            FormCommand::Fill {
                pdf: pdf.clone(),
                values: values.clone(),
                output: None,
            },
        )
        .unwrap();
        let fields = nii_pdf::acroform::read_fields(&std::fs::read(&pdf).unwrap()).unwrap();
        let value = |n: &str| {
            fields
                .iter()
                .find(|f| f.name.ends_with(n))
                .and_then(|f| f.value.clone())
        };
        assert_eq!(
            (value(".f1_14[0]").as_deref(), value(".f1_47[0]").as_deref()),
            (Some("Jane"), Some("72805"))
        );

        std::fs::write(&values, r#"{"f1_14[0]": null}"#).unwrap();
        let err = form(
            &data,
            &ws,
            FormCommand::Fill {
                pdf: pdf.clone(),
                values,
                output: None,
            },
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "f1_14[0]: expected a string, number, or boolean, got null"
        );

        let flat = ws.join("flat.pdf");
        form(
            &data,
            &ws,
            FormCommand::Flatten {
                pdf: pdf.clone(),
                output: flat.clone(),
            },
        )
        .unwrap();
        assert!(
            nii_pdf::acroform::read_fields(&std::fs::read(&flat).unwrap())
                .unwrap()
                .is_empty()
        );

        let png = ws.join("p1.png");
        form(
            &data,
            &ws,
            FormCommand::View {
                pdf,
                page: 1,
                png: Some(png.clone()),
                width: 300,
            },
        )
        .unwrap();
        assert!(std::fs::read(&png).unwrap().starts_with(b"\x89PNG"));
    }

    #[test]
    fn stats_and_fetch_work_on_a_served_index() {
        let dir = testutil::scratch("cli-index");
        testutil::tiny_index(&dir.join("source.db"));
        index::pack(&dir.join("source.db"), &dir.join("served")).unwrap();
        let base = testutil::serve(dir.join("served"));
        let db = dir.join("index.db");
        fetch(&db, Some(format!("{base}/{}", index::ASSET))).unwrap();
        assert!(index::is_ready(&db));
        stats(&db).unwrap();
    }

    fn run_args(args: &[&str]) -> Result<()> {
        run(Cli::try_parse_from(std::iter::once("nii").chain(args.iter().copied())).unwrap())
    }

    #[test]
    fn run_dispatches_corpus_and_form_commands() {
        let dir = testutil::scratch("cli-run");
        let (data, db, ws) = (testutil::data_dir(), dir.join("index.db"), dir.join("ws"));
        let (data, db, ws) = (data.to_str().unwrap(), db.to_str().unwrap(), ws.to_str().unwrap());
        testutil::tiny_index(&dir.join("index.db"));

        let out = dir.join("packed");
        run_args(&["--data-dir", data, "--db", db, "corpus", "stats"]).unwrap();
        run_args(&["--db", db, "corpus", "pack", "--output", out.to_str().unwrap()]).unwrap();
        let base = testutil::serve(out);
        let fetched = dir.join("fetched.db");
        run_args(&[
            "--db",
            fetched.to_str().unwrap(),
            "corpus",
            "fetch",
            "--url",
            &format!("{base}/{}", index::ASSET),
        ])
        .unwrap();
        assert!(index::is_ready(&fetched));

        run_args(&[
            "--data-dir",
            data,
            "--workspace",
            ws,
            "form",
            "fetch",
            "1040",
            "--year",
            "2025",
        ])
        .unwrap();
        assert!(dir.join("ws/2025/forms/f1040.pdf").exists());

        let err = run_args(&["--db", db, "search", " "]).unwrap_err();
        assert_eq!(err.to_string(), "empty query");
        let err = run_args(&["--db", db, "search", "--source", "blog", "gross", "income"]).unwrap_err();
        assert_eq!(err.to_string(), "unknown source 'blog' (usc, cfr, irs-pub, irs-instr)");
        let err = run_args(&["--data-dir", data, "--db", db, "corpus", "build", "--cuda-device", "0"]).unwrap_err();
        assert!(err.to_string().starts_with("a CUDA device was requested"), "{err}");
    }

    #[test]
    fn snippets_cut_on_char_boundaries() {
        assert_eq!(snippet("short", 10), "short");
        assert_eq!(snippet("ab cd ef", 5), "ab cd…");
        assert_eq!(snippet("§§§§", 3), "§…");
    }
}
