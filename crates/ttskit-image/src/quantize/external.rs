//! Runs the user's `pngquant` executable. No shell is involved: the program
//! and its arguments are passed to the operating system separately.

use std::io::{ErrorKind, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use super::QuantizeOptions;
use crate::optimize::StripMode;

/// Name looked up on `PATH` when no program path is given.
const PROGRAM: &str = "pngquant";

/// pngquant's exit code for "quality below the minimum".
const EXIT_QUALITY_TOO_LOW: i32 = 99;

/// Most stderr text kept in an error message.
const STDERR_LIMIT: usize = 2048;

pub(super) enum Run {
    /// Whatever pngquant wrote to stdout (not yet validated).
    Png(Vec<u8>),
    /// Exit code 99, with a message.
    BelowQuality(String),
}

pub(super) enum RunError {
    /// The executable does not exist.
    NotFound(String),
    Failed(String),
}

fn command(program: Option<&Path>) -> Command {
    match program {
        Some(path) => Command::new(path),
        None => Command::new(PROGRAM),
    }
}

/// Arguments in order: options, the palette size, then `-` for stdin.
fn arguments(options: &QuantizeOptions) -> Vec<String> {
    let mut args = Vec::new();
    if let Some((min, max)) = options.quality {
        args.push("--quality".to_owned());
        args.push(format!("{min}-{max}"));
    }
    if options.dither > 0.0 {
        args.push(format!("--floyd={}", options.dither));
    } else {
        args.push("--nofs".to_owned());
    }
    if options.strip != StripMode::None {
        args.push("--strip".to_owned());
    }
    args.push("--output".to_owned());
    args.push("-".to_owned());
    args.push(options.colors.to_string());
    args.push("-".to_owned());
    args
}

fn trimmed_stderr(stderr: &[u8]) -> String {
    let cut = &stderr[..stderr.len().min(STDERR_LIMIT)];
    let text = String::from_utf8_lossy(cut);
    let text = text.trim();
    if stderr.len() > STDERR_LIMIT {
        format!("{text}...")
    } else {
        text.to_owned()
    }
}

/// Runs pngquant on `input` and returns its stdout.
///
/// The input is written from a separate thread while this thread reads stdout
/// and stderr to the end (`wait_with_output`), so neither side can block the
/// other on a full pipe. A program that exits without reading its input
/// (broken pipe) is not an error by itself.
pub(super) fn run(
    program: Option<&Path>,
    input: &[u8],
    options: &QuantizeOptions,
) -> Result<Run, RunError> {
    let mut cmd = command(program);
    cmd.args(arguments(options))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == ErrorKind::NotFound {
            RunError::NotFound(match program {
                Some(path) => format!(
                    "pngquant was not found at {}; install pngquant or pass the path of its executable",
                    path.display()
                ),
                None => "pngquant was not found on PATH; install pngquant or pass the path of its executable"
                    .to_owned(),
            })
        } else {
            RunError::Failed(format!("could not start pngquant: {e}"))
        }
    })?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let output = std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            // Dropping `stdin` at the end closes the pipe.
            match stdin.write_all(input) {
                Err(e) if e.kind() != ErrorKind::BrokenPipe => Err(e),
                _ => Ok(()),
            }
        });
        let output = child.wait_with_output();
        let written = writer.join().expect("writer thread does not panic");
        (output, written)
    });
    let (output, written) = output;
    let output = output.map_err(|e| RunError::Failed(format!("could not run pngquant: {e}")))?;
    match output.status.code() {
        Some(0) => {
            written.map_err(|e| {
                RunError::Failed(format!("could not send the image to pngquant: {e}"))
            })?;
            if output.stdout.is_empty() {
                return Err(RunError::Failed(format!(
                    "pngquant wrote no output{}",
                    detail(&output.stderr)
                )));
            }
            Ok(Run::Png(output.stdout))
        }
        Some(EXIT_QUALITY_TOO_LOW) => {
            let (min, max) = options.quality.unwrap_or((0, 100));
            Ok(Run::BelowQuality(format!(
                "pngquant could not reach the minimum quality {min} (quality {min}-{max})"
            )))
        }
        Some(code) => Err(RunError::Failed(format!(
            "pngquant failed with exit code {code}{}",
            detail(&output.stderr)
        ))),
        None => Err(RunError::Failed(format!(
            "pngquant was terminated by a signal{}",
            detail(&output.stderr)
        ))),
    }
}

fn detail(stderr: &[u8]) -> String {
    let text = trimmed_stderr(stderr);
    if text.is_empty() {
        String::new()
    } else {
        format!(": {text}")
    }
}

#[cfg(test)]
pub(super) fn program_name(program: Option<&Path>) -> std::ffi::OsString {
    command(program).get_program().to_owned()
}
