//! Inertness pin runner (DESIGN §5.6).
//!
//! A probe is a small binary that depends on one of the source cores as a path dependency and
//! prints the pins it measures. Cargo cannot take a dependency path at run time, so this runner
//! writes a throwaway crate under `tools/pin/.work/<system>/`, copies the probe source in, and
//! points its dependency at the source checkout given by `--source`.
//!
//! The source tree is never built in place: every artefact goes to `tools/pin/target/<system>/`.
//!
//! ```text
//! cargo run --manifest-path tools/pin/Cargo.toml -- gb --source <checkout>
//! cargo run --manifest-path tools/pin/Cargo.toml -- nes --source <checkout> --out baselines/pins.toml
//! ```
//!
//! `TENDY_TEST_ROMS` is read by the probe, never by this runner. Arguments after the recognised
//! flags are passed to the probe (the snes probe's `--survey` uses this).

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The crate name a probe writes in `use`, the package that provides it, and the probe file.
fn probe(system: &str) -> Option<(&'static str, &'static str, &'static str)> {
    match system {
        "gb" => Some(("gb", "gb-rust", "gb.rs")),
        "gba" => Some(("gb", "gb-rust", "gba.rs")),
        "nes" => Some(("nes", "nes", "nes.rs")),
        "snes" => Some(("snes", "snes-rust", "snes.rs")),
        // #6's synthetic sound pin: the same SPC700 program through each host's own entry point.
        "spc-snes" => Some(("snes", "snes-rust", "spc_snes.rs")),
        "spc-sgb" => Some(("gb", "gb-rust", "spc_sgb.rs")),
        _ => None,
    }
}

fn usage() -> ! {
    eprintln!(
        "usage: tendy-pin <gb|gba|nes|snes|spc-snes|spc-sgb> --source <source checkout> [--out <pin file>] [probe args...]"
    );
    std::process::exit(2)
}

fn main() {
    let tools = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut args = std::env::args().skip(1);
    let Some(system) = args.next() else { usage() };
    let Some((dependency, package, probe_file)) = probe(&system) else {
        eprintln!("tendy-pin: unknown system {system}");
        usage()
    };

    let mut source: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut probe_args: Vec<String> = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--source" => {
                source = Some(PathBuf::from(args.next().unwrap_or_else(|| usage())));
            }
            "--out" => {
                out = Some(PathBuf::from(args.next().unwrap_or_else(|| usage())));
            }
            _ => probe_args.push(arg),
        }
    }
    let Some(source) = source else {
        eprintln!("tendy-pin: --source is required");
        usage()
    };
    let source = source.canonicalize().unwrap_or_else(|why| {
        eprintln!("tendy-pin: cannot open {}: {why}", source.display());
        std::process::exit(2)
    });
    if !source.join("Cargo.toml").is_file() {
        eprintln!(
            "tendy-pin: {} has no Cargo.toml; --source points at a source checkout",
            source.display()
        );
        std::process::exit(2)
    }

    let work = tools.join(".work").join(&system);
    let src = work.join("src");
    fs::create_dir_all(&src).unwrap_or_else(|why| {
        eprintln!("tendy-pin: cannot create {}: {why}", src.display());
        std::process::exit(1)
    });
    copy_probe(&tools.join("probes/common.rs"), &src.join("common.rs"));
    copy_probe(&tools.join("probes").join(probe_file), &src.join("main.rs"));

    // A path in a TOML basic string needs forward slashes and escaped backslashes; display()
    // gives the platform form, so normalise to `/`. canonicalize() puts a `\\?\` verbatim
    // prefix on Windows paths, which Cargo rejects as a path url, so drop it too.
    let mut dependency_path = source.display().to_string().replace('\\', "/");
    if let Some(stripped) = dependency_path.strip_prefix("//?/") {
        dependency_path = stripped.to_owned();
    }
    let manifest = format!(
        "[package]\n\
         name = \"tendy-pin-probe-{system}\"\n\
         version = \"0.0.0\"\n\
         edition = \"2024\"\n\
         publish = false\n\
         \n\
         [[bin]]\n\
         name = \"tendy-pin-probe-{system}\"\n\
         path = \"src/main.rs\"\n\
         \n\
         [dependencies]\n\
         {dependency} = {{ package = \"{package}\", path = \"{dependency_path}\" }}\n\
         \n\
         [workspace]\n"
    );
    fs::write(work.join("Cargo.toml"), manifest)
        .unwrap_or_else(|why| eprintln!("tendy-pin: cannot write the probe manifest: {why}"));

    let status = run_probe(&tools, &work, &system, &probe_args, out.as_deref());
    if !status.success() {
        eprintln!("tendy-pin: the {system} probe failed");
        std::process::exit(status.code().unwrap_or(1));
    }
}

fn copy_probe(from: &Path, to: &Path) {
    fs::copy(from, to).unwrap_or_else(|why| {
        eprintln!(
            "tendy-pin: cannot copy {} to {}: {why}",
            from.display(),
            to.display()
        );
        std::process::exit(1)
    });
}

/// Runs the probe, streaming its lines to stdout and, when asked, appending them to `out`.
fn run_probe(
    tools: &Path,
    work: &Path,
    system: &str,
    probe_args: &[String],
    out: Option<&Path>,
) -> std::process::ExitStatus {
    // gb and gba both depend on the gb checkout, so they share one target directory (same
    // package, same path: nothing stale can be picked up).
    let target = if system == "gba" { "gb" } else { system };
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .arg("run")
        .arg("--release")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(work.join("Cargo.toml"))
        .arg("--bin")
        .arg(format!("tendy-pin-probe-{system}"))
        .arg("--")
        .args(probe_args)
        .current_dir(tools)
        .env("CARGO_TARGET_DIR", tools.join("target").join(target))
        .stdout(Stdio::piped());
    let mut child = command.spawn().unwrap_or_else(|why| {
        eprintln!("tendy-pin: cannot run cargo: {why}");
        std::process::exit(1)
    });

    let stdout = child.stdout.take().expect("the probe's stdout is piped");
    let mut sink = out.map(|path| {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap_or_else(|why| {
                eprintln!("tendy-pin: cannot open {}: {why}", path.display());
                std::process::exit(1)
            })
    });
    for line in BufReader::new(stdout).lines() {
        let line = line.expect("the probe's stdout reads");
        println!("{line}");
        if let Some(sink) = sink.as_mut() {
            writeln!(sink, "{line}").expect("the --out file writes");
        }
    }
    child.wait().expect("the probe exits")
}
