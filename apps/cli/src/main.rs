mod documents;
mod forms;
mod index;
mod mcp;

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
}

#[derive(Subcommand)]
enum FormCommand {
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
    let cli = Cli::parse();
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
