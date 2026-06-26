//! `golfsetridak` — the Golfsetrið Akureyri website as a single Rust binary
//! built on the AkurAI-Framework crates.
//!
//! This mirrors how the framework's own `crates/cli` serves its `site/` (see
//! `AkurAI-Framework/crates/cli/src/cmd_serve.rs`): an app directory of
//! `frontend/` templates + `backend/` config + `content/` markdown, served by a
//! lean built-in HTTP server with zero external runtime dependencies.
//!
//! Phase 1 (this foundation) serves the static design + the markdown content
//! pages (news, the user handbook, legal) and renders "coming soon" placeholders
//! for the dynamic pages (booking calendar, shop, gift cards, account, admin)
//! that arrive in later phases. See PORT.md.

mod content;
mod mime;
mod serve;

use std::path::PathBuf;
use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("version") | Some("--version") | Some("-V") => {
            println!("golfsetridak {VERSION}");
            Ok(())
        }
        Some("help") | Some("--help") | Some("-h") => {
            print_help();
            Ok(())
        }
        // `serve` is the only real command and also the default. `serve` and a
        // bare invocation both boot the server; flags follow either form.
        _ => {
            let rest = match args.first().map(String::as_str) {
                Some("serve") => &args[1..],
                _ => args,
            };
            let cfg = parse_serve(rest)?;
            serve::run(cfg).map_err(|e| e.to_string())
        }
    }
}

/// Parse flags for `serve`: `--dir`, `--host`, `--port`. Mirrors the framework
/// CLI's hand-rolled parser to keep the zero-dependency promise.
fn parse_serve(args: &[String]) -> Result<serve::Config, String> {
    let mut host = "127.0.0.1".to_string();
    let mut port: u16 = 8090;
    let mut dir = PathBuf::from("frontend");

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--host" => host = next(args, &mut i, "--host")?,
            "--port" => {
                port = next(args, &mut i, "--port")?
                    .parse()
                    .map_err(|_| "invalid --port".to_string())?;
            }
            "--dir" => dir = PathBuf::from(next(args, &mut i, "--dir")?),
            other => return Err(format!("unknown flag '{other}'")),
        }
        i += 1;
    }
    Ok(serve::Config { host, port, dir })
}

fn next(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value"))
}

fn print_help() {
    println!(
        "golfsetridak {VERSION} — Golfsetrið Akureyri, single binary on AkurAI-Framework\n\n\
         USAGE:\n\
         \x20 golfsetridak serve [opts]   Serve the site (also the default command)\n\
         \x20 golfsetridak version        Print version\n\n\
         SERVE OPTIONS:\n\
         \x20 --dir <path>   Frontend directory (default: frontend)\n\
         \x20 --host <addr>  Bind host (default: 127.0.0.1)\n\
         \x20 --port <n>     Bind port (default: 8090)"
    );
}
