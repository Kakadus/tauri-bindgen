#![allow(dead_code, unused_variables)]

mod completions;
mod logger;

use clap::{ArgAction, Parser};
use miette::{bail, IntoDiagnostic, Result, WrapErr};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Instant,
};
use tauri_bindgen_core::GeneratorBuilder;

/// Helper for passing VERSION to opt.
/// If `CARGO_VERSION_INFO` is set, use it, otherwise use `CARGO_PKG_VERSION`.
fn version() -> &'static str {
    option_env!("CARGO_VERSION_INFO").unwrap_or(env!("CARGO_PKG_VERSION"))
}

#[derive(Debug, Parser)]
#[clap(version = version())]
struct Cli {
    #[clap(flatten)]
    common: Common,
    #[clap(subcommand)]
    cmd: Command,
}

#[derive(Debug, Parser)]
enum Command {
    /// Check a definition file for errors.
    Check {
        #[clap(flatten)]
        world: WorldOpt,
    },
    /// Generator for creating bindings that are exposed to the WebView.
    Host(HostGenerator),
    /// Generators for webview libraries.
    #[clap(subcommand)]
    Guest(GuestGenerator),
    /// Print shell completions to stdout
    Completions(completions::Completions),
    /// This generator outputs a Markdown file describing an interface.
    #[cfg(feature = "unstable")]
    Markdown {
        #[clap(flatten)]
        builder: tauri_bindgen_gen_markdown::Builder,
        #[clap(flatten)]
        world: WorldOpt,
    },
    #[cfg(feature = "unstable")]
    Json {
        /// Whether to prettify the generated JSON.
        #[clap(short, long)]
        pretty: bool,
        #[clap(flatten)]
        world: WorldOpt,
    },
}

#[derive(Debug, Parser)]
struct HostGenerator {
    #[clap(flatten)]
    builder: tauri_bindgen_gen_host::Builder,
    #[clap(flatten)]
    world: WorldOpt,
}

#[derive(Debug, Parser)]
enum GuestGenerator {
    /// Generates bindings for Rust guest modules using wasm-bindgen.
    Rust {
        #[clap(flatten)]
        builder: tauri_bindgen_gen_guest_rust::Builder,
        #[clap(flatten)]
        world: WorldOpt,
    },
    /// Generates bindings for JavaScript guest modules.
    Javascript {
        #[clap(flatten)]
        builder: tauri_bindgen_gen_guest_js::Builder,
        #[clap(flatten)]
        world: WorldOpt,
    },
    /// Generates bindings for TypeScript guest modules.
    Typescript {
        #[clap(flatten)]
        builder: tauri_bindgen_gen_guest_ts::Builder,
        #[clap(flatten)]
        world: WorldOpt,
    },
}

#[derive(Debug, Parser)]
struct WorldOpt {
    // #[clap(value_name = "DOCUMENT", value_parser = parse_interface)]
    /// Generate bindings for the WIT document.
    wit: PathBuf,
    /// Names of functions to skip generating bindings for.
    #[clap(long)]
    skip: Vec<String>,
}

#[derive(Debug, Parser, Clone)]
struct Common {
    /// Where to place output files
    #[clap(global = true, long = "out-dir")]
    out_dir: Option<PathBuf>,
    /// Enables verbose logging
    #[clap(short, long, global = true, action = ArgAction::Count)]
    verbose: u8,
}

// A thin wrapper around `run` so we can pretty-print the error
fn main() {
    if let Err(err) = run() {
        log::error!("{err:?}");
    }
}

fn run() -> Result<()> {
    let opt = Cli::parse();

    logger::init(opt.common.verbose);

    let start = Instant::now();

    let out_dir = &opt.common.out_dir.unwrap_or_default();
    match opt.cmd {
        Command::Check { world } => check_interface(world)?,
        Command::Host(HostGenerator { builder, world, .. }) => {
            let (path, contents) = gen_interface(builder, world)?;

            write_file(out_dir, &path, &contents)?;
        }
        Command::Guest(GuestGenerator::Rust { builder, world, .. }) => {
            let (path, contents) = gen_interface(builder, world)?;

            write_file(out_dir, &path, &contents)?;
        }
        Command::Guest(GuestGenerator::Javascript { builder, world, .. }) => {
            let (path, contents) = gen_interface(builder, world)?;

            write_file(out_dir, &path, &contents)?;
        }
        Command::Guest(GuestGenerator::Typescript { builder, world, .. }) => {
            let (path, contents) = gen_interface(builder, world)?;

            write_file(out_dir, &path, &contents)?;
        }
        Command::Completions(opts) => {
            completions::run(&opts)?;
        }
        #[cfg(feature = "unstable")]
        Command::Markdown { builder, world } => {
            let (path, contents) = gen_interface(builder, world)?;

            write_file(out_dir, &path, &contents)?;
        }
        #[cfg(feature = "unstable")]
        Command::Json { world, pretty } => {
            if !world.wit.is_file() {
                bail!("wit file `{}` does not exist", world.wit.display());
            }

            let skipset: HashSet<String, std::collections::hash_map::RandomState> =
                world.skip.into_iter().collect();

            let iface = wit_parser::parse_and_resolve_file(&world.wit, |t| skipset.contains(t))?;

            let stdout = std::io::stdout().lock();
            if pretty {
                serde_json::to_writer_pretty(stdout, &iface).into_diagnostic()?;
            } else {
                serde_json::to_writer(stdout, &iface).into_diagnostic()?;
            }
            println!(); // print a newline for formatting
        }
    };

    log::info!(action = "Finished"; "in {:.2}s", Instant::now().duration_since(start).as_secs_f32());

    Ok(())
}

fn write_file(out_dir: &Path, path: &Path, contents: &str) -> Result<()> {
    let dst = out_dir.join(path);

    log::info!("Generating {dst:?}");
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .into_diagnostic()
            .wrap_err(format!("failed to create {parent:?}"))?;
    }
    std::fs::write(&dst, contents)
        .into_diagnostic()
        .wrap_err(format!("failed to write {dst:?}"))?;

    Ok(())
}

fn gen_interface<B>(builder: B, opts: WorldOpt) -> Result<(PathBuf, String)>
where
    B: GeneratorBuilder,
{
    if !opts.wit.is_file() {
        bail!("wit file `{}` does not exist", opts.wit.display());
    }

    let skipset: HashSet<String, std::collections::hash_map::RandomState> =
        opts.skip.into_iter().collect();

    let iface = wit_parser::parse_and_resolve_file(&opts.wit, |t| skipset.contains(t))?;

    let mut gen = builder.build(iface);

    Ok(gen.to_file())
}

fn check_interface(opts: WorldOpt) -> Result<()> {
    log::info!(action = "Checking"; "{}", opts.wit.to_string_lossy());

    if !opts.wit.is_file() {
        bail!("wit file `{}` does not exist", opts.wit.display());
    }

    let skipset: HashSet<String, std::collections::hash_map::RandomState> =
        opts.skip.into_iter().collect();

    wit_parser::parse_and_resolve_file(&opts.wit, |t| skipset.contains(t))?;

    Ok(())
}
