//! Chaff from the command line (#57).
//!
//! # Why this exists
//!
//! The GPU lives on a machine that may have no desktop session — the 3090 box runs headless
//! overnight. Indexing, face detection and tagging are all engine work with no UI in them,
//! and requiring a window server to run them means the fastest machine in the house is the
//! one that cannot do the work.
//!
//! # Why it is a separate binary and not a flag on the app
//!
//! `src-tauri` links the webview. A `--headless` flag on it would still need
//! `webkit2gtk` installed, still need a display at startup on some platforms, and would
//! still be a GUI binary pretending not to be one. This depends on `chaff-core` and
//! `chaff-faces` and nothing else, so it builds and runs on a bare server.
//!
//! # The rule it follows
//!
//! **It never writes to the library except through the trash**, and it never deletes
//! anything. `index`, `faces` and `tags` read photographs and write a catalog. `sidecars`
//! writes XMP and is explicit. There is no `delete` subcommand and there will not be one: a
//! destructive operation belongs behind a dialog that shows what will move.

use std::path::{Path, PathBuf};

use chaff_core::catalog::{open, store};
use chaff_core::pipeline;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "chaff", about = "Cull photographs. Index, group faces, tag — no GUI.")]
struct Cli {
    /// The catalog database.
    ///
    /// Defaults to `chaff.db` beside the working directory rather than the desktop app's
    /// app-data location: a headless run should not silently share a catalog with a GUI
    /// session, and the two would fight over the file.
    #[arg(long, global = true, env = "CHAFF_DB", default_value = "chaff.db")]
    db: PathBuf,

    /// More output. Repeat for more.
    #[arg(long, short, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Index a library and score it.
    Index {
        /// The library root.
        root: PathBuf,
        /// Score every photograph again, ignoring the measurement cache.
        #[arg(long)]
        rescore: bool,
    },
    /// Find faces and group them.
    Faces {
        root: PathBuf,
    },
    /// Tag photographs with a vision model.
    Tags {
        root: PathBuf,
        /// How many to do in this run. The work list is the catalog, so it is resumable.
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// The endpoint. Read from the environment when not given, so a script does not
        /// carry an address.
        #[arg(long, env = "CHAFF_VLM")]
        endpoint: Option<String>,
        #[arg(long, env = "CHAFF_VLM_MODEL", default_value = "chaff-vlm")]
        model: String,
    },
    /// Ask a vision endpoint what actually works.
    Diagnose {
        #[arg(long, env = "CHAFF_VLM")]
        endpoint: Option<String>,
        #[arg(long, env = "CHAFF_VLM_MODEL", default_value = "chaff-vlm")]
        model: String,
    },
    /// What is in a library.
    Stats {
        root: PathBuf,
    },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level)).init();

    match run(&cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            // The message, not a backtrace. This is a tool someone runs from a script at
            // three in the morning, and the useful output is what went wrong.
            eprintln!("chaff: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = open(&cli.db)?;

    match &cli.command {
        Command::Index { root, rescore } => {
            let now = now_seconds();
            let report = if *rescore {
                pipeline::index_and_score_forced(&mut conn, root, now, &mut progress)?
            } else {
                pipeline::index_and_score_with_progress(&mut conn, root, now, &mut progress)?
            };
            println!(
                "{} photographs · {} files scanned · {} scored · {} unscoreable in {:.1}s",
                report.photos,
                report.scanned_files,
                report.scored,
                report.unscoreable,
                report.elapsed_ms as f64 / 1000.0
            );
            for problem in report.inconsistencies() {
                // Printed, not swallowed: an inconsistency is the catalog disagreeing with
                // the disk, and a headless run is exactly where nobody would notice.
                eprintln!("  inconsistent: {problem}");
            }
            Ok(())
        }

        Command::Faces { root } => {
            let library_id = library_at(&conn, root)?;
            let now = now_seconds();
            let mut last = 0usize;
            let report = chaff_faces::pass::run(&mut conn, &data_dir(), library_id, now, &mut |done, total| {
                if done / 100 != last / 100 {
                    eprintln!("  {done}/{total} files");
                    last = done;
                }
            })?;
            println!(
                "{} files · {} faces · {} embedded · {} unreadable · {} groups in {:.1}s",
                report.detected_files,
                report.faces_found,
                report.embedded,
                report.unreadable,
                report.people,
                report.elapsed_ms as f64 / 1000.0
            );
            Ok(())
        }

        Command::Tags { root, limit, endpoint, model } => {
            let Some(base) = endpoint.clone() else {
                return Err("no endpoint: pass --endpoint or set CHAFF_VLM".into());
            };
            let library_id = library_at(&conn, root)?;
            let now = now_seconds();
            let report = chaff_core::tagging::run(
                &mut conn,
                library_id,
                &chaff_core::vlm::Endpoint { base, model: model.clone() },
                *limit,
                now,
                &mut |_, _| {},
            )?;
            println!(
                "{} tagged · {} tags · {} tokens · {} to go in {:.1}s",
                report.tagged,
                report.tags,
                report.completion_tokens,
                report.remaining,
                report.elapsed_ms as f64 / 1000.0
            );
            if let Some(reason) = report.stopped_because {
                // A distinct exit-worthy condition: the run did not finish.
                return Err(format!("stopped early: {reason}").into());
            }
            Ok(())
        }

        Command::Diagnose { endpoint, model } => {
            let Some(base) = endpoint.clone() else {
                return Err("no endpoint: pass --endpoint or set CHAFF_VLM".into());
            };
            let e = chaff_core::vlm::Endpoint { base, model: model.clone() };
            let policy = chaff_core::egress::Policy::default();
            let mut ok = true;

            match chaff_core::vlm::health(&policy, &e, 10) {
                Ok(()) => println!("  health      ok"),
                Err(err) => {
                    println!("  health      FAILED: {err}");
                    ok = false;
                }
            }
            match chaff_core::vlm::list_models(&policy, &e, 10) {
                Ok(models) => {
                    let has = models.iter().any(|m| m == model);
                    println!("  models      {:?}{}", models, if has { "" } else { "  (configured model missing)" });
                    ok &= has;
                }
                Err(err) => {
                    println!("  models      FAILED: {err}");
                    ok = false;
                }
            }

            if !ok {
                return Err("the endpoint is not usable for tagging".into());
            }
            println!("\n  {model} is reachable and advertises the configured model.");
            Ok(())
        }

        Command::Stats { root } => {
            let library_id = library_at(&conn, root)?;
            let photos = store::photos(&conn, library_id)?;
            let dirs = store::directories(&conn, library_id)?;
            let people = store::people(&conn, library_id)?;
            let tags = store::tag_counts(&conn, library_id, None)?;

            println!("  library     {}", root.display());
            println!("  photographs {}", photos.len());
            println!("  folders     {}", dirs.len());
            println!("  groups      {} ({} named)", people.len(), people.iter().filter(|p| p.name.is_some()).count());
            println!("  tags        {}", tags.len());
            // Scored, from the catalog's own count rather than by inspecting rows the
            // stats query does not need to load.
            let scored = store::scored_count(&conn, library_id).unwrap_or(0);
            println!("  scored      {scored}");
            Ok(())
        }
    }
}

/// A progress line on stderr, so stdout stays parseable.
fn progress(p: pipeline::Progress) {
    match p {
        pipeline::Progress::Scanning { files } => eprintln!("  scanning: {files} files"),
        pipeline::Progress::Scoring { done, total, .. } => {
            if done % 500 == 0 || done == total {
                eprintln!("  scoring: {done}/{total}");
            }
        }
        pipeline::Progress::Ranking { photographs } => eprintln!("  ranking {photographs}"),
    }
}

fn library_at(
    conn: &chaff_core::rusqlite::Connection,
    root: &Path,
) -> Result<i64, Box<dyn std::error::Error>> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let root_str = root.to_string_lossy().to_string();
    store::library_id_for_root(conn, &root_str)?
        .ok_or_else(|| format!("{} is not indexed yet — run `chaff index` first", root.display()).into())
}

/// Where downloaded models live, for a headless run.
///
/// `CHAFF_DATA` or beside the catalog. The desktop app uses the platform's app-data
/// directory; a server run has no such notion, and putting models beside the catalog keeps
/// one `--db` flag sufficient to relocate everything.
fn data_dir() -> PathBuf {
    std::env::var("CHAFF_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
