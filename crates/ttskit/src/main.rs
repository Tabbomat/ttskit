mod assets;
mod check_urls;
mod cloud;
mod diff;
mod editor;
mod fetch_assets;
mod http;
mod image;
mod lint;
mod mcp;
mod rehost;
mod rewrite_urls;
mod save;
mod watch;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use editor::CliError;
use std::time::SystemTime;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use ttskit_cloud::{BackupStore, CloudBackend};

/// Unofficial command line toolkit for Tabletop Simulator mod development.
#[derive(Parser)]
#[command(version, about, arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage the Steam Cloud files of Tabletop Simulator (unofficial)
    Cloud {
        #[command(subcommand)]
        command: CloudCommand,
    },
    /// Inspect, check, convert, resize, optimize and quantize image files, and build or split deck sheets (local only; needs a build with the `image` feature; unofficial)
    Image {
        #[command(subcommand)]
        command: ImageCommand,
    },
    /// Work with save files (local only, no Steam needed; unofficial)
    Save {
        #[command(subcommand)]
        command: SaveCommand,
    },
    /// Talk to a running Tabletop Simulator through its External Editor API (unofficial)
    Editor {
        #[command(subcommand)]
        command: EditorCommand,
    },
    /// Run a Model Context Protocol server on stdio that exposes the editor API to AI assistants (unofficial)
    ///
    /// Stdout is reserved for the protocol; diagnostics go to stderr. The
    /// server holds the editor port for its whole life and buffers what the
    /// game sends; it does not start if the port is in use. Exits when stdin
    /// closes.
    Mcp(McpCli),
    /// Push scripts and UI to the running game whenever the unpacked tree changes
    ///
    /// Watches DIR (written by `save unpack`) and the --include directories.
    /// After a change (debounced) the scripts and UI are bundled as `save pack`
    /// does and sent with Save & Play. This reloads the whole game and discards
    /// unsaved changes in it, so running watch counts as consent; scripts or UI
    /// missing in the source are deleted in the game. Only scripts and UI are
    /// pushed: other changes are reported and need a full pack and load.
    /// Errors are printed and the watch keeps running. The game's `print`
    /// output and Lua errors are shown. Ctrl+C stops.
    Watch(WatchCli),
    /// Print a shell completion script to stdout
    ///
    /// See the README for how to install it for your shell.
    Completions(CompletionsCli),
}

#[derive(clap::Args)]
struct CompletionsCli {
    /// The shell to generate the script for
    #[arg(value_enum)]
    shell: Shell,
}

#[derive(clap::Args)]
struct McpCli {
    /// Project directory; save_and_play may only read paths inside it (default: the current directory)
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,
    /// Keep at most this many events (print, error, custom, saved, loaded) for get_console; the buffer also holds at most 4 MiB of text
    #[arg(long, value_name = "EVENTS", default_value_t = 1000, value_parser = clap::value_parser!(u32).range(1..))]
    console_buffer: u32,
    #[command(flatten)]
    ports: EditorPorts,
}

#[derive(clap::Args)]
struct WatchCli {
    /// The directory written by `save unpack`
    dir: PathBuf,
    /// Inline `require("Name")` and `<Include src="Name"/>` from this directory
    /// and watch it too; repeatable, searched in order
    #[arg(long, value_name = "LIB_DIR")]
    include: Vec<PathBuf>,
    /// The project config whose manifest resolves `asset:{PATH}` placeholders
    /// [default: ttskit.toml in the current directory if it exists]
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Push once and exit (for editor tasks), then show the game's console
    /// output until --quiet-ms pass without a message
    #[arg(long)]
    once: bool,
    /// Also write the packed save to this file on every change (atomically;
    /// an existing file is overwritten, and only when its content changes)
    #[arg(long, value_name = "SAVE.json")]
    pack_to: Option<PathBuf>,
    /// Milliseconds without file changes before a push starts
    #[arg(long, default_value_t = watch::DEFAULT_DEBOUNCE.as_millis() as u64, value_name = "MILLISECONDS")]
    debounce_ms: u64,
    #[command(flatten)]
    quiet: QuietWindow,
    #[command(flatten)]
    ports: EditorPorts,
}

/// How long to keep listening after the last message (shared by `exec` and
/// `console`).
#[derive(clap::Args)]
struct QuietWindow {
    /// Stop after this many milliseconds without a message
    #[arg(long, default_value_t = 1000, value_name = "MILLISECONDS")]
    quiet_ms: u64,
}

/// Ports of the External Editor API.
#[derive(clap::Args)]
struct EditorPorts {
    /// Port the game listens on
    #[arg(long, default_value_t = ttskit_editor::GAME_PORT)]
    game_port: u16,
    /// Port this command listens on for the game's replies
    #[arg(long, default_value_t = ttskit_editor::EDITOR_PORT)]
    editor_port: u16,
    /// Seconds to wait for connecting, sending and for the game's reply
    #[arg(long, default_value_t = ttskit_editor::DEFAULT_TIMEOUT.as_secs(), value_name = "SECONDS")]
    timeout: u64,
}

impl EditorPorts {
    fn endpoints(&self) -> editor::Endpoints {
        let mut ep = editor::Endpoints::new(self.game_port, self.editor_port);
        ep.timeout = std::time::Duration::from_secs(self.timeout);
        ep
    }
}

#[derive(Subcommand)]
enum EditorCommand {
    /// List the objects with a script or UI in the loaded game (read-only)
    GetScripts(EditorGetScriptsCli),
    /// Send the scripts and UI of an unpacked save to the game and reload it (Save & Play)
    ///
    /// Reloads the whole game and discards unsaved changes in it. Only objects
    /// whose script or UI differ from the game are sent, each with both fields
    /// in full. Without --yes the changes are shown and nothing is sent.
    SaveAndPlay(EditorSaveAndPlayCli),
    /// Run Lua in the game and print what it reports
    ///
    /// Runs in the context of an object (default: Global). The object must
    /// already have a script, or the game reports an error. The game was not
    /// observed to answer `return` statements, so use `print(...)` for output;
    /// a returned value is shown if the game sends one. Output printed by the
    /// game's own scripts in the meantime is included. A Lua error makes the
    /// command fail.
    Exec(EditorExecCli),
    /// Print the game's `print` output and Lua errors, stamped on arrival
    ///
    /// Without --follow, listens until --quiet-ms pass without a message (or
    /// --timeout seconds in total) and exits 0; with --follow, runs until
    /// Ctrl+C. Only messages that arrive while this runs are seen, and it holds
    /// the editor port, so other `editor` commands cannot run meanwhile. The
    /// game sends no timestamp and no origin for `print`; a Lua error shows the
    /// GUID of the failing object. Lua errors are output and do not change the
    /// exit status. `--json` prints one JSON object per line.
    Console(EditorConsoleCli),
    /// Send a custom message to the game's `onExternalMessage` handler
    ///
    /// The message must be a JSON object; the game ignores anything else.
    /// The game does not acknowledge it, so success only means it was sent.
    Send(EditorSendCli),
}

#[derive(clap::Args)]
struct EditorGetScriptsCli {
    /// Print the full scripts and UI as JSON instead of a summary
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    ports: EditorPorts,
}

#[derive(clap::Args)]
struct EditorSaveAndPlayCli {
    /// A directory written by `save unpack`; objects are matched by GUID
    dir: PathBuf,
    /// Inline `require("Name")` and `<Include src="Name"/>` from this
    /// directory, as `save pack --include` does; repeatable
    #[arg(long, value_name = "LIB_DIR")]
    include: Vec<PathBuf>,
    /// The project config whose manifest resolves `asset:{PATH}` placeholders
    /// [default: ttskit.toml in the current directory if it exists]
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Show the changes and send nothing
    #[arg(long)]
    dry_run: bool,
    /// Really send the changes (required; the game reloads)
    #[arg(long)]
    yes: bool,
    /// Allow scripts or UI that exist in the game but not in the source to be deleted
    #[arg(long)]
    allow_delete: bool,
    /// Print machine-readable JSON instead of text
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    ports: EditorPorts,
}

#[derive(clap::Args)]
struct EditorExecCli {
    /// The Lua code to run
    code: String,
    /// GUID of the object to run in; the default `-1` is Global
    #[arg(long, default_value = ttskit_editor::GLOBAL_GUID)]
    guid: String,
    #[command(flatten)]
    quiet: QuietWindow,
    /// Print machine-readable JSON instead of text
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    ports: EditorPorts,
}

#[derive(clap::Args)]
struct EditorConsoleCli {
    /// Keep running until interrupted instead of exiting when output stops
    #[arg(long, conflicts_with_all = ["quiet_ms"])]
    follow: bool,
    /// Print one JSON object per line instead of text
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    quiet: QuietWindow,
    #[command(flatten)]
    ports: EditorPorts,
}

#[derive(clap::Args)]
struct EditorSendCli {
    /// A JSON object, for example '{"action":"ping"}'
    message: String,
    /// Print machine-readable JSON instead of text
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    ports: EditorPorts,
}

#[derive(Subcommand)]
enum ImageCommand {
    /// Show what images contain and whether Tabletop Simulator can use them (read-only)
    ///
    /// Reports format, size, colour layout, bit depth, frames and ICC profile,
    /// then problems: errors (the game rejects the file, e.g. gif, bmp, tiff,
    /// CMYK JPEG, animated WebP), warnings (it loads but not as expected, e.g.
    /// no fully opaque pixel so no token can be made) and notes. Takes files
    /// and directories (scanned recursively for png, jpg, webp, gif, bmp and
    /// tiff); output is sorted by path. Exits with status 1 if a file could not
    /// be read or has errors; warnings and notes do not change the exit status.
    Info(ImageInfoCli),
    /// Report images that are larger than the game can use well (read-only)
    ///
    /// Reads only the file header where it can (format, size, colour layout)
    /// and reports per file: size, file size, format and whether it has an
    /// alpha channel. Findings, each with a rule id: side-too-long (a side
    /// over --max-side, default 4096: the largest size seen to load fine in
    /// the game, 8192 loaded slowly; an observed limit, not a documented one),
    /// file-too-large (over --max-bytes; no default limit), extension-mismatch
    /// (the extension does not match the content; the game decides by content,
    /// but other tools may not) and alpha-opaque (an alpha channel in which
    /// every pixel is fully opaque; only checked for png and webp, which means
    /// decoding them). Takes files and directories (scanned recursively).
    /// Exit status: 0 without findings, 2 with findings, 1 if a file could not
    /// be read (this wins over 2). To fix oversized images, see `image resize`.
    Check(ImageCheckCli),
    /// Convert images to PNG or JPEG, the formats Tabletop Simulator loads
    ///
    /// Reads png, jpg, webp, bmp, tiff and gif (decided by content, not by
    /// extension) and writes PNG or JPEG. An animated input converts its first
    /// frame, with a warning. PNG output keeps alpha and 16-bit channels; JPEG
    /// output is 8-bit and refuses an image with transparent or translucent
    /// pixels unless --background says what to flatten it onto. A CMYK JPEG
    /// becomes RGB. The colour profile and EXIF data are dropped (the game
    /// ignores them; an EXIF orientation is applied to the pixels first)
    /// unless --keep-metadata is given. Without --out or --out-dir the output
    /// goes next to each input with the new extension. Existing files are
    /// never overwritten without --yes, and the whole run is refused up front
    /// if two inputs would write the same file. Each file is converted
    /// independently: a failure is reported and the other files still convert.
    /// Output is deterministic. Exits with status 1 if any file failed.
    Convert(ImageConvertCli),
    /// Shrink PNG files without changing a pixel
    ///
    /// Re-encodes each PNG losslessly with oxipng (filters, compression, colour
    /// type and bit depth reductions). The decoded pixels, alpha included, stay
    /// identical; this is checked for every file. A file is only replaced when
    /// the result is smaller: in place, a file that did not shrink is left
    /// untouched; with --out or --out-dir, the original bytes are copied to the
    /// output. Files that are not PNG (decided by content) are skipped with a
    /// note. You must say where results go: --out FILE (one input), --out-dir
    /// DIR, or --in-place with --yes; --dry-run alone runs in memory and only
    /// reports the savings. Existing outputs are never overwritten without
    /// --yes. --strip safe (default) removes text, EXIF and similar chunks but
    /// keeps the colour profile, pixel density and animation; all also removes
    /// the colour profile (an animated PNG is then skipped); none keeps every
    /// chunk. Output is deterministic for the same level, strip mode and ttskit
    /// build, so an optimized file stays stable; a ttskit upgrade (new oxipng
    /// version) may produce other bytes. Exits with status 1 if any file failed.
    Optimize(ImageOptimizeCli),
    /// Scale PNG and JPEG images down, keeping the aspect ratio
    ///
    /// Give exactly one size: --max-side N (the longer side becomes N),
    /// --width N, --height N (the other side follows) or --scale F. An image
    /// is never enlarged unless --allow-enlarge is given: one that would grow
    /// is left as it is (a copy of the original goes to --out or --out-dir),
    /// and so is one that already has the target size. The default filter is
    /// lanczos3; --filter picks another (nearest keeps pixel art sharp).
    /// Transparency is resampled without dark or light fringes, and 16-bit
    /// PNG stays 16-bit. PNG stays PNG and JPEG stays JPEG (re-encoded at
    /// --quality, default 85, so a resized JPEG loses a little more quality);
    /// any other format is skipped with a note, run `image convert` first. The
    /// colour profile and EXIF data are dropped (the game ignores them; an
    /// EXIF orientation is applied to the pixels first); an animated PNG is
    /// resized by its first frame, with a warning. You must say where results
    /// go: --out FILE (one input), --out-dir DIR, or --in-place with --yes;
    /// --dry-run alone runs in memory and only reports the new sizes.
    /// Existing outputs are never overwritten without --yes. Output is
    /// deterministic. Exits with status 1 if any file failed.
    Resize(ImageResizeCli),
    /// Build deck sheets from card images, or cut them into cards
    ///
    /// A custom deck is one or more grid sheets of card images. `deck build`
    /// assembles single card images into such sheets, `deck split` cuts a sheet back into cards.
    Deck {
        #[command(subcommand)]
        command: DeckCommand,
    },
    /// Reduce PNG files to at most 256 colours (lossy)
    ///
    /// Turns each PNG into an indexed PNG with a palette of at most --colors
    /// entries (alpha included), then runs the lossless optimizer over it. This
    /// is LOSSY: pixels change, and the colour profile, pixel density, text and
    /// other chunks are not kept (the file is re-encoded; 16-bit becomes 8-bit).
    /// The built-in engine is deterministic. --engine pngquant (or --pngquant
    /// PATH) runs your own pngquant executable instead, which usually looks
    /// better, especially with smooth transparency; it is not shipped with
    /// ttskit and its output depends on its version. --quality MIN-MAX sets a
    /// quality floor and target (0 to 100): a file that cannot reach MIN is
    /// skipped and left alone, and gets no output file. Animated PNGs and files
    /// that are not PNG are skipped too. A result is only used when it is
    /// smaller: in place, a file that did not shrink is left untouched; with
    /// --out or --out-dir, the original bytes are copied to the output. You
    /// must say where results go: --out FILE (one input), --out-dir DIR, or
    /// --in-place with --yes; --dry-run alone runs in memory and only reports
    /// the savings. Existing outputs are never overwritten without --yes.
    /// Exits with status 1 if any file failed or pngquant cannot be run.
    Quantize(ImageQuantizeCli),
    /// Build the images of the project config's `[[image.pipeline]]` rules
    ///
    /// Reads the rules from `[[image.pipeline]]` in ttskit.toml (source glob,
    /// output directory, steps such as resize, optimize, quantize and convert,
    /// and an optional deck). Only outputs whose inputs, options, ttskit
    /// version or file changed are rebuilt; what was built is recorded in
    /// ttskit-image.lock.json next to ttskit.toml. Existing outputs are
    /// overwritten. Sources are never modified. Outputs that no rule makes any
    /// more are listed as stale and only deleted with --prune --yes, and only
    /// files that ttskit wrote and that are unchanged. Conflicting outputs
    /// (two sources to one file, or an output that is a source) block the
    /// whole build before anything is written. --dry-run prints the plan and
    /// writes nothing, not even the lock file. Exits with status 1 if a job
    /// failed. The usual flow is `ttskit image build`, then
    /// `ttskit cloud sync push`.
    Build(ImageBuildCli),
}

#[derive(clap::Args)]
struct ImageInfoCli {
    /// Image files or directories
    #[arg(required = true, value_name = "PATH")]
    inputs: Vec<PathBuf>,
    /// Print one JSON object with all results
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct ImageCheckCli {
    /// Image files or directories
    #[arg(required = true, value_name = "PATH")]
    inputs: Vec<PathBuf>,
    /// Longest allowed side in pixels
    #[arg(long, value_name = "N", default_value_t = 4096, value_parser = clap::value_parser!(u32).range(1..))]
    max_side: u32,
    /// Largest allowed file size in bytes; k, m and g suffixes mean 1024-based units (e.g. 2m)
    #[arg(long, value_name = "BYTES", value_parser = image::parse_bytes)]
    max_bytes: Option<u64>,
    /// Print one JSON object with all results
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct ImageConvertCli {
    /// Image files or directories (scanned recursively)
    #[arg(required = true, value_name = "INPUT")]
    inputs: Vec<PathBuf>,
    /// Output format
    #[arg(long, value_enum)]
    to: image::Target,
    /// JPEG quality, 1 to 100 [default: 85]
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u8).range(1..=100))]
    quality: Option<u8>,
    /// Colour to flatten transparency onto for JPEG output: #RRGGBB, #RGB, white or black
    #[arg(long, value_name = "COLOR")]
    background: Option<String>,
    /// Keep the colour profile and EXIF data
    #[arg(long)]
    keep_metadata: bool,
    /// Output file (for exactly one input)
    #[arg(long, value_name = "FILE", conflicts_with = "out_dir")]
    out: Option<PathBuf>,
    /// Output directory (created if missing); files are named like their input
    #[arg(long, value_name = "DIR")]
    out_dir: Option<PathBuf>,
    /// Overwrite existing output files
    #[arg(long)]
    yes: bool,
    /// Show what would be written, but write nothing
    #[arg(long)]
    dry_run: bool,
    /// Print one JSON object with all results
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct ImageOptimizeCli {
    /// PNG files or directories (scanned recursively)
    #[arg(required = true, value_name = "INPUT")]
    inputs: Vec<PathBuf>,
    /// Effort from 0 (fastest) to 6 (smallest, slowest)
    #[arg(long, value_name = "N", default_value_t = 2, value_parser = clap::value_parser!(u8).range(0..=6))]
    level: u8,
    /// Metadata to remove: safe, all or none
    #[arg(long, value_enum, default_value = "safe")]
    strip: image::Strip,
    /// Output file (for exactly one input)
    #[arg(long, value_name = "FILE", conflicts_with_all = ["out_dir", "in_place"])]
    out: Option<PathBuf>,
    /// Output directory (created if missing); files are named like their input
    #[arg(long, value_name = "DIR", conflicts_with = "in_place")]
    out_dir: Option<PathBuf>,
    /// Replace the input files (needs --yes)
    #[arg(long)]
    in_place: bool,
    /// Confirm --in-place, or overwrite existing output files
    #[arg(long)]
    yes: bool,
    /// Optimize in memory and report the savings, but write nothing
    #[arg(long)]
    dry_run: bool,
    /// Print one JSON object with all results
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
#[command(group(clap::ArgGroup::new("size").required(true).multiple(false).args(["max_side", "width", "height", "scale"])))]
struct ImageResizeCli {
    /// Image files or directories (scanned recursively)
    #[arg(required = true, value_name = "INPUT")]
    inputs: Vec<PathBuf>,
    /// Make the longer side this many pixels
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    max_side: Option<u32>,
    /// Make the width this many pixels
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    width: Option<u32>,
    /// Make the height this many pixels
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    height: Option<u32>,
    /// Multiply both sides by this factor, e.g. 0.5
    #[arg(long, value_name = "F", value_parser = image::parse_scale)]
    scale: Option<f64>,
    /// Allow results larger than the input
    #[arg(long)]
    allow_enlarge: bool,
    /// Resampling filter
    #[arg(long, value_enum, default_value = "lanczos3")]
    filter: image::FilterKind,
    /// JPEG quality, 1 to 100 [default: 85]
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u8).range(1..=100))]
    quality: Option<u8>,
    /// Output file (for exactly one input)
    #[arg(long, value_name = "FILE", conflicts_with_all = ["out_dir", "in_place"])]
    out: Option<PathBuf>,
    /// Output directory (created if missing); files are named like their input
    #[arg(long, value_name = "DIR", conflicts_with = "in_place")]
    out_dir: Option<PathBuf>,
    /// Replace the input files (needs --yes)
    #[arg(long)]
    in_place: bool,
    /// Confirm --in-place, or overwrite existing output files
    #[arg(long)]
    yes: bool,
    /// Resize in memory and report the new sizes, but write nothing
    #[arg(long)]
    dry_run: bool,
    /// Print one JSON object with all results
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct ImageQuantizeCli {
    /// PNG files or directories (scanned recursively)
    #[arg(required = true, value_name = "INPUT")]
    inputs: Vec<PathBuf>,
    /// Largest palette size, 2 to 256
    #[arg(long, value_name = "N", default_value_t = 256, value_parser = clap::value_parser!(u16).range(2..=256))]
    colors: u16,
    /// Quality floor and target, MIN-MAX, each 0 to 100 (e.g. 60-90)
    #[arg(long, value_name = "MIN-MAX", value_parser = image::parse_quality)]
    quality: Option<(u8, u8)>,
    /// Dithering strength, 0.0 (none) to 1.0 (full)
    #[arg(long, value_name = "F", default_value_t = 1.0, value_parser = image::parse_dither)]
    dither: f32,
    /// Effort of the final lossless pass, 0 (fastest) to 6 (smallest, slowest)
    #[arg(long, value_name = "N", default_value_t = 2, value_parser = clap::value_parser!(u8).range(0..=6))]
    level: u8,
    /// Metadata to remove: safe, all or none
    #[arg(long, value_enum, default_value = "safe")]
    strip: image::Strip,
    /// Quantizer: builtin (default) or pngquant
    #[arg(long, value_enum)]
    engine: Option<image::EngineKind>,
    /// Path of the pngquant executable (implies --engine pngquant)
    #[arg(long, value_name = "PATH")]
    pngquant: Option<PathBuf>,
    /// Output file (for exactly one input)
    #[arg(long, value_name = "FILE", conflicts_with_all = ["out_dir", "in_place"])]
    out: Option<PathBuf>,
    /// Output directory (created if missing); files are named like their input
    #[arg(long, value_name = "DIR", conflicts_with = "in_place")]
    out_dir: Option<PathBuf>,
    /// Replace the input files (needs --yes)
    #[arg(long)]
    in_place: bool,
    /// Confirm --in-place, or overwrite existing output files
    #[arg(long)]
    yes: bool,
    /// Quantize in memory and report the savings, but write nothing
    #[arg(long)]
    dry_run: bool,
    /// Print one JSON object with all results
    #[arg(long)]
    json: bool,
}

/// Arguments of `image build`, in their own struct so that clap builds them
/// in a function of their own.
#[derive(clap::Args)]
struct ImageBuildCli {
    /// The project config; its directory is the base of all paths in it
    #[arg(
        long,
        value_name = "PATH",
        default_value = ttskit_cloud::ProjectConfig::FILE_NAME
    )]
    config: PathBuf,
    /// Print the plan and write nothing
    #[arg(long)]
    dry_run: bool,
    /// Also delete stale outputs (needs --yes)
    #[arg(long)]
    prune: bool,
    /// Confirm --prune
    #[arg(long)]
    yes: bool,
    /// Print one JSON object with the plan and the results
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
enum DeckCommand {
    /// Assemble card images into the grid sheets of a custom deck
    ///
    /// Takes card files and directories (a directory gives its image files,
    /// not those of subdirectories, in natural order: card2 before card10;
    /// files keep the order you give them and may repeat) and/or --list FILE
    /// (one path per line, blank lines and lines starting with # ignored,
    /// relative paths relative to the list). Writes NAME.png (NAME_1.png,
    /// NAME_2.png, ... for several sheets) and NAME.json into --out-dir; the
    /// JSON describes the deck: sheet files, grids and the slot of every card
    /// (unofficial; the rules are in docs/deck-sheets.md). Slots count from 0,
    /// left to right and top to bottom. A sheet has at most 10 columns and 7
    /// rows. Unless --no-hidden-slot is given, the last slot holds the
    /// hidden-card image (--hidden FILE, else the --fill colour), so a 10x7
    /// sheet holds 69 cards (70 with --no-hidden-slot); more cards go on
    /// further sheets. The grid is the smallest square-ish one, at least 2x2,
    /// unless --columns and/or --rows fix it. All cards must have the size of
    /// the first card (or --card-size); other sizes are an error unless --fit
    /// contain, cover or stretch says how to fit them. --max-sheet-side N
    /// scales the cards down so no sheet side exceeds N px. PNG output is RGB
    /// unless a pixel is transparent; JPEG output flattens transparency onto
    /// --fill. The colour profile and EXIF data are dropped (an EXIF
    /// orientation is applied first). Existing outputs are never overwritten
    /// without --yes, and the run is refused up front if outputs collide or
    /// overwrite an input. --dry-run builds in memory and writes nothing.
    /// Output is deterministic. Exits with status 1 on any error.
    Build(DeckBuildCli),
    /// Write the saved-object JSON of a deck built by `deck build`
    ///
    /// Reads DESCRIPTION, the NAME.json of `deck build`, and writes FILE, an
    /// object file for Tabletop Simulator (unofficial; see
    /// docs/deck-sheets.md): a `DeckCustom` with one `CustomDeck` entry per
    /// sheet, `DeckIDs` and one card per card (card id = sheet number times
    /// 100 plus the slot), or a single `CardCustom` for a one-card deck.
    /// The sheets must already be uploaded. Give each face sheet as a URL
    /// with --face-url (once per sheet, in order), or leave it out: the
    /// sheets named in the description (relative to its folder) are then
    /// looked up by local path in the project manifest (--config, default
    /// ttskit.toml in the current directory; the manifest is the one
    /// `cloud sync push` and `cloud upload` keep). A sheet that changed
    /// after its upload is refused. --back-url is a URL or the local path of
    /// an uploaded back image; with --unique-backs give one per sheet.
    /// BackIsHidden comes from the description (`deck build --no-hidden-slot`
    /// sets it). --meta FILE sets the Nickname, Description and GMNotes of
    /// the deck and of each card: JSON {"deck": {"nickname", "description",
    /// "gm_notes"}, "cards": [...]}, where cards is a list in deck order
    /// (null for none) or an object keyed by the card's source path in the
    /// description or by its file name; every key is optional. Without a
    /// nickname there, --nicknames-from-files names a card after its source
    /// file, otherwise the nickname is empty. GUIDs are derived from the
    /// content, so output is identical between runs. An existing FILE is
    /// not overwritten without --yes. --dry-run writes nothing. Exits with
    /// status 1 on any error.
    Object(DeckObjectCli),
    /// Cut a deck sheet into single card images (the reverse of `deck build`)
    ///
    /// Writes NAME_00.png, NAME_01.png, ... into --out-dir, one per card slot
    /// in number order (slots count from 0, left to right and top to bottom),
    /// and NAME_hidden.png for the hidden-card slot (unofficial; the rules are
    /// in docs/deck-sheets.md). NAME is the file name of SHEET without its
    /// extension unless --name says otherwise; the slot number has at least 2
    /// digits. The grid comes from exactly one of: --columns and --rows;
    /// --description FILE, the NAME.json of `deck build` (the sheet is found by
    /// its file name; the card count and the hidden-card slot are taken from
    /// it); --save FILE --deck-id ID, the CustomDeck entry of a save (the
    /// last slot is the hidden-card slot unless BackIsHidden is true). With
    /// --columns and --rows the last slot is assumed to be the hidden-card
    /// slot, as in `deck build`; --no-hidden-slot says all slots are cards and
    /// overrides the description or save, --skip-hidden does not write the
    /// hidden-card slot. --count N writes only the first N cards. The sheet
    /// must divide evenly into the grid. Cards are written as PNG by default
    /// (the pixels and 16-bit depth are kept exactly, whatever the sheet's
    /// format) or --to jpg, which flattens transparency onto --fill. The
    /// colour profile and EXIF data are dropped (an EXIF orientation is
    /// applied first). Existing outputs are never overwritten without --yes,
    /// and the run is refused up front if outputs collide or overwrite an
    /// input. --dry-run cuts in memory and writes nothing. Output is
    /// deterministic. Exits with status 1 on any error.
    Split(DeckSplitCli),
}

#[derive(clap::Args)]
struct DeckBuildCli {
    /// Card image files or directories (not scanned recursively)
    #[arg(value_name = "CARD")]
    cards: Vec<PathBuf>,
    /// Text file with one card file or directory per line (after the CARD arguments)
    #[arg(long, value_name = "FILE")]
    list: Option<PathBuf>,
    /// Directory for the sheets and the description (created if missing)
    #[arg(long, value_name = "DIR")]
    out_dir: Option<PathBuf>,
    /// File name of the output, without extension
    #[arg(long, value_name = "NAME", default_value = "deck")]
    name: String,
    /// Output format of the sheets
    #[arg(long, value_enum, default_value = "png")]
    to: image::Target,
    /// JPEG quality, 1 to 100 [default: 85]
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u8).range(1..=100))]
    quality: Option<u8>,
    /// Columns of every sheet, 1 to 10 (the limit of the game)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=10))]
    columns: Option<u32>,
    /// Rows of every sheet, 1 to 7 (the limit of the game)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=7))]
    rows: Option<u32>,
    /// Size of a card in pixels, e.g. 300x420 [default: the size of the first card]
    #[arg(long, value_name = "WxH", value_parser = image::parse_card_size)]
    card_size: Option<(u32, u32)>,
    /// How a card of another size is fitted: contain, cover or stretch [default: refuse]
    #[arg(long, value_enum)]
    fit: Option<image::FitKind>,
    /// Image for the hidden-card slot (the last slot of every sheet)
    #[arg(long, value_name = "FILE")]
    hidden: Option<PathBuf>,
    /// Use the last slot for a card too (the deck must then use a separate back image)
    #[arg(long)]
    no_hidden_slot: bool,
    /// Colour of unused slots, of --fit contain bands and of flattened transparency: #RRGGBB, #RGB, white or black
    #[arg(long, value_name = "COLOR", default_value = "808080")]
    fill: String,
    /// Scale the cards down so that no sheet side exceeds this many pixels
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    max_sheet_side: Option<u32>,
    /// Overwrite existing output files
    #[arg(long)]
    yes: bool,
    /// Build in memory and report the plan, but write nothing
    #[arg(long)]
    dry_run: bool,
    /// Print one JSON object with all results
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct DeckObjectCli {
    /// The description file (NAME.json) written by `deck build`
    #[arg(value_name = "DESCRIPTION")]
    description: PathBuf,
    /// The object file to write
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    /// URL (or local path of an uploaded file) of a face sheet; once per sheet [default: look the sheets up in the manifest]
    #[arg(long = "face-url", value_name = "URL")]
    face_urls: Vec<String>,
    /// URL (or local path of an uploaded file) of the card back; with --unique-backs once per sheet
    #[arg(long = "back-url", value_name = "URL")]
    back_urls: Vec<String>,
    /// Every card has its own back: one back sheet per face sheet (UniqueBack)
    #[arg(long)]
    unique_backs: bool,
    /// The project config for the manifest lookup [default: ttskit.toml in the current directory if it exists]
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// JSON file with the name, description and GM notes of the deck and its cards
    #[arg(long, value_name = "FILE")]
    meta: Option<PathBuf>,
    /// Name each card after its source file (without extension)
    #[arg(long)]
    nicknames_from_files: bool,
    /// Overwrite an existing output file
    #[arg(long)]
    yes: bool,
    /// Build the object and report it, but write nothing
    #[arg(long)]
    dry_run: bool,
    /// Print one JSON object with all results
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
#[command(group(clap::ArgGroup::new("grid_source").required(true).multiple(false).args(["columns", "description", "save"])))]
struct DeckSplitCli {
    /// The sheet image (PNG, JPEG or WebP)
    #[arg(value_name = "SHEET")]
    sheet: PathBuf,
    /// Directory for the card images (created if missing)
    #[arg(long, value_name = "DIR")]
    out_dir: Option<PathBuf>,
    /// Start of the output file names [default: the file name of SHEET without extension]
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
    /// Output format of the cards
    #[arg(long, value_enum, default_value = "png")]
    to: image::Target,
    /// JPEG quality, 1 to 100 [default: 85]
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u8).range(1..=100))]
    quality: Option<u8>,
    /// Colour that transparency is flattened onto for --to jpg: #RRGGBB, #RGB, white or black
    #[arg(long, value_name = "COLOR", default_value = "808080")]
    fill: String,
    /// Columns of the sheet, 1 to 10 (with --rows; the last slot is assumed to be the hidden-card slot)
    #[arg(long, value_name = "N", requires = "rows", value_parser = clap::value_parser!(u32).range(1..=10))]
    columns: Option<u32>,
    /// Rows of the sheet, 1 to 7 (with --columns)
    #[arg(long, value_name = "N", requires = "columns", value_parser = clap::value_parser!(u32).range(1..=7))]
    rows: Option<u32>,
    /// Description file written by `deck build`: grid, card count and hidden-card slot of SHEET
    #[arg(long, value_name = "FILE")]
    description: Option<PathBuf>,
    /// Save file (JSON) with the CustomDeck entry of the sheet (needs --deck-id)
    #[arg(long, value_name = "FILE", requires = "deck_id")]
    save: Option<PathBuf>,
    /// Key of the CustomDeck entry in the save, e.g. 1 (needs --save)
    #[arg(long, value_name = "ID", requires = "save", conflicts_with_all = ["columns", "description"])]
    deck_id: Option<String>,
    /// Write only the first N card slots [default: all, or the card count of the description]
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    count: Option<u32>,
    /// The last slot is a card too (overrides the description or save)
    #[arg(long)]
    no_hidden_slot: bool,
    /// Do not write the hidden-card slot
    #[arg(long, conflicts_with = "no_hidden_slot")]
    skip_hidden: bool,
    /// Overwrite existing output files
    #[arg(long)]
    yes: bool,
    /// Cut in memory and report the plan, but write nothing
    #[arg(long)]
    dry_run: bool,
    /// Print one JSON object with all results
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
enum SaveCommand {
    /// Unpack a save file into a directory of scripts, UI, state and objects
    Unpack(SaveUnpackCli),
    /// Pack an unpacked directory back into a save file
    Pack(SavePackCli),
    /// Check which asset URLs of a save no longer load (needs the network)
    ///
    /// Requests every distinct URL once (HEAD, or a ranged GET for hosts that
    /// refuse HEAD), follows redirects and retries timeouts, network errors,
    /// 429 and 5xx. Results: ok, redirect (the target loads), dead (HTTP
    /// error), unreachable (network error or timeout) and skipped (excluded
    /// by --host / --skip-host). Exit status: 0 when nothing is dead or
    /// unreachable, 2 when something is, 1 on errors.
    CheckUrls(SaveCheckUrlsCli),
    /// Download the asset URLs of a save into a local directory (needs the network)
    ///
    /// Downloads every distinct http(s) URL that is not yet in --out and
    /// records it in <out>/index.json; files are stored as
    /// <out>/files/<sha256>.<type>. Rerunning downloads only what is missing
    /// (--verify rehashes the files already there). HTML pages answered
    /// instead of a file are rejected. With --cache-dir (the game's Mods
    /// folder) files found there are copied first and only the rest is
    /// downloaded; --offline then never uses the network. The save is not
    /// modified. Exit status: 0 when everything is present, 2 when a URL failed (the rest is still
    /// downloaded) or the index could not be written, 1 on errors.
    FetchAssets(SaveFetchAssetsCli),
    /// Replace asset URLs in a save from a mapping (local only)
    ///
    /// The mapping is a TSV of `old URL<TAB>new URL` (--map; blank lines and
    /// lines starting with # are skipped), and/or --from-manifest: a Steam
    /// Cloud URL of the save whose SHA1 is in the project manifest gets the
    /// manifest's URL. --normalize-steam-hosts also rewrites the legacy
    /// http://cloud-3.steamusercontent.com/ugc/ prefix to the current one
    /// (the rest of the URL is kept). Only whole URL values are replaced;
    /// URLs inside Lua scripts and XML UI are counted and reported
    /// separately, and replaced only with --in-scripts. Mapping lines never
    /// chain, and the same old URL with two different new URLs is an error.
    /// The plan (per line: occurrences, objects, lines that match nothing)
    /// is printed first; --dry-run stops there. The new save goes to --out
    /// (an existing file needs --yes); replacing the input needs --in-place
    /// --yes. Everything except the replaced strings stays as it was.
    RewriteUrls(SaveRewriteUrlsCli),
    /// Move assets from other hosts into the project's Steam Cloud and rewrite the save
    ///
    /// Run inside a project (ttskit.toml). Selects every http(s) asset URL of
    /// the save except those already in your own cloud (--host and --skip-host
    /// narrow that; `imgur` stands for imgur.com), downloads the files into
    /// <local dir>/<folder>/files (like fetch-assets), uploads and shares
    /// them in the cloud folder <cloud folder>/<folder>/files, records them in
    /// the project manifest, and writes a copy of the save that uses the new
    /// URLs. The plan comes first: every URL with its size (looked up with
    /// HEAD, so some may be unknown) and the free cloud quota; --dry-run stops
    /// there and downloads nothing, otherwise you are asked (--yes skips the
    /// question). Identical content is uploaded once. A run that stopped can
    /// be repeated: files already downloaded or already in the cloud are not
    /// fetched or uploaded again. The save is written only after all uploads
    /// succeeded, to SAVE.rehosted.json unless --out is given; replacing the
    /// input needs --in-place --yes. URLs that fail to download stay as they
    /// are (exit status 2). Only rehost assets you may redistribute. Add
    /// <folder>/index.json to [local] ignore so `cloud sync push` does not
    /// upload the download index.
    Rehost(SaveRehostCli),
    /// Check a save for common mistakes (local only; no network, no game)
    ///
    /// Rules cover GUIDs (duplicates, missing), object positions, non-finite
    /// numbers, URL fields (empty, malformed, http, legacy Steam host, local
    /// paths), Lua syntax (Lua 5.2 grammar only; `!=` and `|x| expr`, which
    /// the game accepts, are reported), XML UI well-formedness, script state
    /// size and JSON, and deck sizes and card ids. --list-rules prints them
    /// with their default severity. Settings come from the [lint] table of
    /// ttskit.toml in the current directory (or --config), then --allow
    /// (off), --warn and --deny (severity) override single rules. Only
    /// findings at or above --level (default warning) are shown and counted.
    /// Findings are grouped by owning object. Exit status: 0 when nothing is
    /// shown, 2 when there are findings, 1 on errors (unreadable save, bad
    /// config, unknown rule id).
    Lint(SaveLintCli),
    /// Show which objects changed between two saves (local only)
    ///
    /// Objects are matched by GUID, also inside containers and states, and
    /// reported as added, removed, moved to another container or changed
    /// (transform, script, XML UI, script state, asset URLs, other fields by
    /// JSON path). Global script, UI, table, sky, lighting, tab states and
    /// custom UI assets are compared too. Numbers are compared as text unless
    /// --tolerance is given (transform values only). Objects with a missing or
    /// repeated GUID are reported as ambiguous and paired by position in
    /// their parent list. Unpack both saves and use a file diff to review
    /// scripts line by line. Exit status: 0 when equal, 2 when different, 1
    /// on errors.
    Diff(SaveDiffCli),
}

#[derive(clap::Args)]
struct SaveDiffCli {
    /// The old save file (JSON)
    old: PathBuf,
    /// The new save file (JSON)
    new: PathBuf,
    /// Treat transform numbers closer than this as equal [default: exact text]
    #[arg(long, value_name = "EPSILON", default_value = "0")]
    tolerance: String,
    /// Show the full line diff of changed scripts and XML UI
    #[arg(long)]
    scripts: bool,
    /// List every change with old and new values
    #[arg(long, short)]
    verbose: bool,
    /// Print machine-readable JSON instead of text
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct SaveLintCli {
    /// The save file (JSON)
    #[arg(required_unless_present = "list_rules")]
    save: Option<PathBuf>,
    /// Project config with a [lint] table [default: ./ttskit.toml if it exists]
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Switch a rule off; repeatable
    #[arg(long, value_name = "RULE")]
    allow: Vec<String>,
    /// Report a rule as a warning; repeatable
    #[arg(long, value_name = "RULE")]
    warn: Vec<String>,
    /// Report a rule as an error; repeatable
    #[arg(long, value_name = "RULE")]
    deny: Vec<String>,
    /// Lowest severity to show and count: info, warning or error
    #[arg(long, value_name = "LEVEL", default_value = "warning")]
    level: String,
    /// Print machine-readable JSON instead of text
    #[arg(long)]
    json: bool,
    /// Print every rule with its default severity and exit
    #[arg(long)]
    list_rules: bool,
}

#[derive(clap::Args)]
struct SaveRewriteUrlsCli {
    /// The save file (JSON)
    save: PathBuf,
    /// Mapping file: one `old URL<TAB>new URL` per line
    #[arg(long, value_name = "FILE")]
    map: Option<PathBuf>,
    /// Project manifest (ttskit.manifest.json): replace Steam Cloud URLs by the manifest's
    /// URL for the same SHA1
    #[arg(long, value_name = "FILE")]
    from_manifest: Option<PathBuf>,
    /// Rewrite the legacy Steam Cloud host (cloud-3.steamusercontent.com) to the current one
    #[arg(long)]
    normalize_steam_hosts: bool,
    /// Also replace URLs inside LuaScript, LuaScriptState and XmlUI strings
    #[arg(long)]
    in_scripts: bool,
    /// Write the new save here
    #[arg(long, value_name = "FILE", conflicts_with = "in_place")]
    out: Option<PathBuf>,
    /// Replace the input file (needs --yes)
    #[arg(long)]
    in_place: bool,
    /// Confirm replacing the input or overwriting --out
    #[arg(long)]
    yes: bool,
    /// Print the plan and write nothing
    #[arg(long)]
    dry_run: bool,
    /// Print the plan and the result as one JSON object
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct SaveRehostCli {
    /// The save file (JSON)
    save: PathBuf,
    /// The project config
    #[arg(
        long,
        value_name = "PATH",
        default_value = ttskit_cloud::ProjectConfig::FILE_NAME
    )]
    config: PathBuf,
    /// Sub folder of the project's cloud folder and local directory for the rehosted files
    #[arg(long, value_name = "SUB", default_value = rehost::DEFAULT_FOLDER)]
    folder: String,
    /// Only rehost URLs on this host or its subdomains; repeatable (`imgur` means imgur.com)
    #[arg(long, value_name = "HOST")]
    host: Vec<String>,
    /// Leave URLs on this host or its subdomains alone; repeatable
    #[arg(long, value_name = "HOST")]
    skip_host: Vec<String>,
    /// Write the new save here [default: SAVE.rehosted.json next to the input]
    #[arg(long, value_name = "FILE", conflicts_with = "in_place")]
    out: Option<PathBuf>,
    /// Replace the input file (needs --yes)
    #[arg(long)]
    in_place: bool,
    /// Also replace URLs inside LuaScript, LuaScriptState and XmlUI strings
    #[arg(long)]
    in_scripts: bool,
    /// Do not ask for confirmation; also confirms --in-place and overwriting --out
    #[arg(long)]
    yes: bool,
    /// Print the plan and change nothing (nothing is downloaded either)
    #[arg(long, conflicts_with = "yes")]
    dry_run: bool,
    /// Largest accepted file: bytes, optionally with a k, m or g suffix (binary multiples)
    #[arg(long, value_name = "SIZE", default_value = "512m", value_parser = |s: &str| fetch_assets::parse_size(s))]
    max_size: u64,
    /// Most requests in flight at once
    #[arg(long, value_name = "N", default_value_t = 16, value_parser = clap::value_parser!(u32).range(1..=256))]
    concurrency: u32,
    /// Most requests in flight at once to one host
    #[arg(long, value_name = "N", default_value_t = 4, value_parser = clap::value_parser!(u32).range(1..=64))]
    per_host: u32,
    /// Extra attempts after a timeout, network error, 429 or 5xx
    #[arg(long, value_name = "N", default_value_t = 3, value_parser = clap::value_parser!(u32).range(0..=10))]
    retries: u32,
    /// Timeout for connecting, in seconds (a whole download may take up to an hour)
    #[arg(long, value_name = "SECONDS", default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=600))]
    timeout: u32,
    /// Directory for the index backups [default: <local app data>/ttskit/cloud-backups]
    #[arg(long, env = "TTSKIT_BACKUP_DIR", value_name = "DIR")]
    backup_dir: Option<PathBuf>,
}
#[derive(clap::Args)]
struct SaveFetchAssetsCli {
    /// The save file (JSON)
    save: PathBuf,
    /// The output directory (created if missing; may already hold an earlier backup)
    #[arg(long, value_name = "DIR")]
    out: PathBuf,
    /// Print the plan and download nothing; creates no files
    #[arg(long)]
    dry_run: bool,
    /// Rehash the files already present instead of only comparing their size
    #[arg(long)]
    verify: bool,
    /// The game's Mods folder (with the Images, Models, ... subfolders): files found there are
    /// copied instead of downloaded
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,
    /// Never use the network: only take files from --cache-dir; others fail
    #[arg(long, requires = "cache_dir")]
    offline: bool,
    /// Largest accepted file: bytes, optionally with a k, m or g suffix (binary multiples)
    #[arg(long, value_name = "SIZE", default_value = "512m", value_parser = |s: &str| fetch_assets::parse_size(s))]
    max_size: u64,
    /// Only download URLs on this host or its subdomains; repeatable
    #[arg(long, value_name = "HOST")]
    host: Vec<String>,
    /// Skip URLs on this host or its subdomains; repeatable
    #[arg(long, value_name = "HOST")]
    skip_host: Vec<String>,
    /// Most downloads in flight at once
    #[arg(long, value_name = "N", default_value_t = 16, value_parser = clap::value_parser!(u32).range(1..=256))]
    concurrency: u32,
    /// Most downloads in flight at once from one host
    #[arg(long, value_name = "N", default_value_t = 4, value_parser = clap::value_parser!(u32).range(1..=64))]
    per_host: u32,
    /// Extra attempts after a timeout, network error, 429 or 5xx
    #[arg(long, value_name = "N", default_value_t = 3, value_parser = clap::value_parser!(u32).range(0..=10))]
    retries: u32,
    /// Timeout for connecting, in seconds (a whole download may take up to an hour)
    #[arg(long, value_name = "SECONDS", default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=600))]
    timeout: u32,
    /// Print machine-readable JSON instead of text
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct SaveCheckUrlsCli {
    /// The save file (JSON)
    save: PathBuf,
    /// Also download the start of each file and report HTML pages (some hosts
    /// answer 200 with an error page) as dead
    #[arg(long)]
    verify_content: bool,
    /// Only check URLs on this host or its subdomains; repeatable
    #[arg(long, value_name = "HOST")]
    host: Vec<String>,
    /// Skip URLs on this host or its subdomains; repeatable
    #[arg(long, value_name = "HOST")]
    skip_host: Vec<String>,
    /// Most requests in flight at once
    #[arg(long, value_name = "N", default_value_t = 16, value_parser = clap::value_parser!(u32).range(1..=256))]
    concurrency: u32,
    /// Most requests in flight at once to one host
    #[arg(long, value_name = "N", default_value_t = 4, value_parser = clap::value_parser!(u32).range(1..=64))]
    per_host: u32,
    /// Timeout of one request in seconds
    #[arg(long, value_name = "SECONDS", default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..=600))]
    timeout: u32,
    /// Extra attempts after a timeout, network error, 429 or 5xx
    #[arg(long, value_name = "N", default_value_t = 3, value_parser = clap::value_parser!(u32).range(0..=10))]
    retries: u32,
    /// Print machine-readable JSON instead of text
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct SaveUnpackCli {
    /// The save file (JSON)
    save: PathBuf,
    /// The output directory. It must be missing or empty unless --force is given;
    /// the path is never searched or guessed
    #[arg(short, long, value_name = "DIR")]
    output: PathBuf,
    /// Write into a non-empty directory: files unpack creates are overwritten,
    /// all other files are left alone [alias: --yes]
    #[arg(long, visible_alias = "yes")]
    force: bool,
    /// With --force: also delete files a previous unpack created (listed in
    /// .ttskit/unpack.json) that this unpack no longer creates; never deletes
    /// anything else
    #[arg(long, requires = "force")]
    clean: bool,
    /// Print the files that would be created, changed or removed and write nothing
    #[arg(long)]
    dry_run: bool,
    /// Replace the URLs of pushed assets (from the manifest of the project)
    /// by `asset:{PATH}` placeholders; only the exact URL text is mapped
    #[arg(long)]
    assets: bool,
    /// With --assets: the project config [default: ttskit.toml in the current directory if it exists]
    #[arg(long, value_name = "FILE", requires = "assets")]
    config: Option<PathBuf>,
    /// Print machine-readable JSON instead of text
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct SavePackCli {
    /// The directory written by `save unpack`
    dir: PathBuf,
    /// The save file to write; it must not exist yet unless --force is given
    #[arg(short, long, value_name = "FILE")]
    output: PathBuf,
    /// Overwrite an existing output file [alias: --yes]
    #[arg(long, visible_alias = "yes")]
    force: bool,
    /// Print whether the output file would be created or changed and write nothing
    #[arg(long)]
    dry_run: bool,
    /// Keep the line endings recorded at unpack (default: LF everywhere)
    #[arg(long)]
    keep_eol: bool,
    /// Inline `require("Name")` (Name.lua, Name.ttslua) and `<Include src="Name"/>`
    /// (Name.xml) from this directory; repeatable, searched in order
    #[arg(long, value_name = "LIB_DIR")]
    include: Vec<PathBuf>,
    /// The project config whose manifest resolves `asset:{PATH}` placeholders
    /// [default: ttskit.toml in the current directory if it exists]
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Resolve placeholders of assets that changed since they were pushed
    /// (with their old URL and a warning) instead of failing
    #[arg(long)]
    allow_stale: bool,
    /// Print machine-readable JSON instead of text
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
enum CloudCommand {
    /// List the folders and files in the Steam Cloud (read-only)
    List(CloudListCli),
    /// Restore the cloud index from a local backup (dry run unless --yes)
    Restore(CloudRestoreCli),
    /// Upload a file to the Steam Cloud, share it and print its URL
    Upload(CloudUploadCli),
    /// Delete a file or folder from the Steam Cloud (dry run unless --yes)
    Delete(CloudDeleteCli),
    /// Compare a local directory with a cloud folder (read-only)
    Status(CloudStatusCli),
    /// Make a cloud folder match a local directory (unofficial)
    Sync {
        #[command(subcommand)]
        command: SyncCommand,
    },
    /// Print the project manifest (local only, no Steam needed)
    Manifest(CloudManifestCli),
    /// Add a `filename<TAB>URL` list of already shared files to the manifest
    ImportTsv(CloudImportTsvCli),
    /// List the asset URLs of a save file, grouped by host (local only, no Steam needed)
    Scan(CloudScanCli),
    /// Print the download URL of a cloud file
    Url(CloudUrlCli),
}

#[derive(clap::Args)]
struct CloudListCli {
    /// Only show this folder and its subfolders (separator `/`)
    #[arg(long, value_name = "FOLDER")]
    folder: Option<String>,
    /// Print machine-readable JSON instead of a tree
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct CloudRestoreCli {
    /// Backup to restore; omit to list the backups
    backup: Option<String>,
    /// Restore for real; without it only the plan is printed
    #[arg(long)]
    yes: bool,
    /// Directory holding the backups [default: <local app data>/ttskit/cloud-backups]
    #[arg(long, env = "TTSKIT_BACKUP_DIR", value_name = "DIR")]
    backup_dir: Option<PathBuf>,
}

#[derive(clap::Args)]
struct CloudUploadCli {
    /// The local file to upload
    file: PathBuf,
    /// Target cloud folder (separator `/`); default is the root
    #[arg(long, value_name = "FOLDER")]
    folder: Option<String>,
    /// Name in the cloud; default is the file name
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
    /// Only print the plan
    #[arg(long, conflicts_with = "yes")]
    dry_run: bool,
    /// Do not ask for confirmation
    #[arg(long)]
    yes: bool,
    /// Print machine-readable JSON instead of text
    #[arg(long)]
    json: bool,
    /// The project config; its directory gets the manifest. Default: ttskit.toml in the current directory if it exists
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Directory for the index backups [default: <local app data>/ttskit/cloud-backups]
    #[arg(long, env = "TTSKIT_BACKUP_DIR", value_name = "DIR")]
    backup_dir: Option<PathBuf>,
}

#[derive(clap::Args)]
#[command(group(clap::ArgGroup::new("what").required(true).args(["name", "folder"])))]
struct CloudDeleteCli {
    /// Folder path and name (`Decks/back.png`), display name, or cloud name
    name: Option<String>,
    /// Delete this folder with all its files and subfolders (separator `/`)
    #[arg(long, value_name = "FOLDER")]
    folder: Option<String>,
    /// Delete for real; without it only the plan is printed
    #[arg(long)]
    yes: bool,
    /// Print machine-readable JSON instead of text
    #[arg(long)]
    json: bool,
    /// Directory for the index backups [default: <local app data>/ttskit/cloud-backups]
    #[arg(long, env = "TTSKIT_BACKUP_DIR", value_name = "DIR")]
    backup_dir: Option<PathBuf>,
}

#[derive(clap::Args)]
struct CloudStatusCli {
    /// The local directory; its subdirectories map to cloud subfolders
    dir: PathBuf,
    /// Cloud folder to compare with (separator `/`); default is the root
    #[arg(long, value_name = "FOLDER")]
    folder: Option<String>,
    /// Print machine-readable JSON instead of text
    #[arg(long)]
    json: bool,
    /// The project config; its [local] ignore patterns apply, relative to DIR. Default: ttskit.toml in the current directory if it exists
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

#[derive(clap::Args)]
struct CloudManifestCli {
    /// Print the `filename<TAB>URL` list instead of JSON
    #[arg(long, conflicts_with = "json")]
    tsv: bool,
    /// Print the manifest file content (the default)
    #[arg(long)]
    json: bool,
    /// The project config; its directory holds the manifest
    #[arg(
        long,
        value_name = "PATH",
        default_value = ttskit_cloud::ProjectConfig::FILE_NAME
    )]
    config: PathBuf,
}

#[derive(clap::Args)]
struct CloudImportTsvCli {
    /// The text file with one `filename<TAB>URL` per line
    file: PathBuf,
    /// The project config; its directory holds the manifest
    #[arg(
        long,
        value_name = "PATH",
        default_value = ttskit_cloud::ProjectConfig::FILE_NAME
    )]
    config: PathBuf,
    /// Only print the plan
    #[arg(long, conflicts_with = "yes")]
    dry_run: bool,
    /// Do not ask for confirmation
    #[arg(long)]
    yes: bool,
    /// Print machine-readable JSON instead of text
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct CloudScanCli {
    /// The save file (JSON)
    save: PathBuf,
    /// Also add the files of your own Steam Cloud that the save uses to the manifest
    #[arg(long)]
    adopt: bool,
    /// The project config; its directory holds the manifest
    #[arg(
        long,
        value_name = "PATH",
        default_value = ttskit_cloud::ProjectConfig::FILE_NAME,
        requires = "adopt"
    )]
    config: PathBuf,
    /// Only print the plan
    #[arg(long, requires = "adopt", conflicts_with = "yes")]
    dry_run: bool,
    /// Do not ask for confirmation
    #[arg(long, requires = "adopt")]
    yes: bool,
    /// Print machine-readable JSON instead of text
    #[arg(long)]
    json: bool,
    /// In text output, list the path, nickname and GUID of every occurrence
    /// (JSON always includes them)
    #[arg(long)]
    occurrences: bool,
}

#[derive(clap::Args)]
struct CloudUrlCli {
    /// Folder path and name (`Decks/back.png`), display name, or cloud name
    name: String,
    /// Share the file first if it has no URL yet
    #[arg(long)]
    share: bool,
    /// Only print the plan
    #[arg(long, conflicts_with = "yes")]
    dry_run: bool,
    /// Do not ask for confirmation
    #[arg(long)]
    yes: bool,
    /// Print machine-readable JSON instead of text
    #[arg(long)]
    json: bool,
    /// Directory for the index backups [default: <local app data>/ttskit/cloud-backups]
    #[arg(long, env = "TTSKIT_BACKUP_DIR", value_name = "DIR")]
    backup_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum SyncCommand {
    /// Upload the local directory to the cloud folder named in the project config
    Push(SyncPushCli),
    /// Download the cloud folder named in the project config into the local directory
    Pull(SyncPullCli),
}

#[derive(clap::Args)]
struct SyncPushCli {
    /// The project config
    #[arg(
        long,
        value_name = "PATH",
        default_value = ttskit_cloud::ProjectConfig::FILE_NAME
    )]
    config: PathBuf,
    /// Also replace changed files and delete cloud files that are not local (files matching [local] ignore are left alone)
    #[arg(long)]
    prune: bool,
    /// Only print the plan
    #[arg(long, conflicts_with = "yes")]
    dry_run: bool,
    /// Do not ask for confirmation
    #[arg(long)]
    yes: bool,
    /// Print machine-readable JSON instead of text
    #[arg(long)]
    json: bool,
    /// Directory for the index backups [default: <local app data>/ttskit/cloud-backups]
    #[arg(long, env = "TTSKIT_BACKUP_DIR", value_name = "DIR")]
    backup_dir: Option<PathBuf>,
}

#[derive(clap::Args)]
struct SyncPullCli {
    /// The project config
    #[arg(
        long,
        value_name = "PATH",
        default_value = ttskit_cloud::ProjectConfig::FILE_NAME
    )]
    config: PathBuf,
    /// Also move local files that are not in the cloud folder to the recycle bin (files matching [local] ignore are left alone)
    #[arg(long)]
    prune: bool,
    /// Only print the plan
    #[arg(long, conflicts_with = "yes")]
    dry_run: bool,
    /// Do not ask for confirmation
    #[arg(long)]
    yes: bool,
    /// Print machine-readable JSON instead of text
    #[arg(long)]
    json: bool,
}

/// Asks on the terminal; the prompt goes to stderr so that stdout stays clean.
fn ask_on_terminal(prompt: &str) -> Result<bool, cloud::CommandError> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Err(cloud::CommandError::ConfirmationNeeded);
    }
    eprint!("{prompt} [y/N] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    stdin.read_line(&mut line)?;
    let answer = line.trim();
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

#[cfg(feature = "steam")]
fn connect() -> Result<Box<dyn CloudBackend>, String> {
    ttskit_cloud::SteamCloud::connect()
        .map(|cloud| Box::new(cloud) as Box<dyn CloudBackend>)
        .map_err(|e| e.to_string())
}

#[cfg(not(feature = "steam"))]
fn connect() -> Result<Box<dyn CloudBackend>, String> {
    Err(
        "this ttskit build has no Steam support; rebuild with `cargo build --features steam`"
            .to_owned(),
    )
}

/// The directory holding the project config.
fn config_dir_of(config: &Path) -> &Path {
    match config.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn run_simple(command: Command) -> Result<(), CliError> {
    match command {
        Command::Completions(CompletionsCli { shell }) => {
            let mut out = std::io::stdout().lock();
            clap_complete::generate(shell, &mut Cli::command(), "ttskit", &mut out);
            out.flush().map_err(|e| e.to_string())?;
            Ok(())
        }
        Command::Mcp(McpCli {
            root,
            console_buffer,
            ports,
        }) => mcp::run(root.as_deref(), ports.endpoints(), console_buffer as usize),
        Command::Watch(WatchCli {
            dir,
            include,
            config,
            once,
            pack_to,
            debounce_ms,
            quiet,
            ports,
        }) => {
            let options = watch::WatchOptions {
                dir,
                includes: include,
                config,
                once,
                pack_to,
                quiet: std::time::Duration::from_millis(quiet.quiet_ms),
                debounce: std::time::Duration::from_millis(debounce_ms),
            };
            // Unlocked handles: they are Send, the locked ones are not.
            let mut out = std::io::stdout();
            let mut err = std::io::stderr();
            watch::run(options, ports.endpoints(), &mut out, &mut err)?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Editor {
            command: EditorCommand::GetScripts(EditorGetScriptsCli { json, ports }),
        } => {
            let mut out = std::io::stdout().lock();
            editor::get_scripts(ports.endpoints(), json, &mut out)?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Editor {
            command:
                EditorCommand::SaveAndPlay(EditorSaveAndPlayCli {
                    dir,
                    include,
                    config,
                    dry_run,
                    yes,
                    allow_delete,
                    json,
                    ports,
                }),
        } => {
            // Read and pack the tree before touching the game.
            let desired = editor::desired_from_tree(&dir, include, config.as_deref())?;
            let mut out = std::io::stdout().lock();
            let options = editor::SaveAndPlayOptions {
                dry_run,
                yes,
                allow_delete,
                json,
            };
            editor::save_and_play(&desired, ports.endpoints(), options, &mut out)?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Editor {
            command:
                EditorCommand::Exec(EditorExecCli {
                    code,
                    guid,
                    quiet,
                    json,
                    ports,
                }),
        } => {
            let mut out = std::io::stdout().lock();
            let wait = std::time::Duration::from_millis(quiet.quiet_ms);
            editor::exec(&code, &guid, wait, json, ports.endpoints(), &mut out)?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Editor {
            command:
                EditorCommand::Console(EditorConsoleCli {
                    follow,
                    json,
                    quiet,
                    ports,
                }),
        } => {
            let mode = if follow {
                editor::ConsoleMode::Follow
            } else {
                editor::ConsoleMode::Once {
                    quiet: std::time::Duration::from_millis(quiet.quiet_ms),
                }
            };
            let mut out = std::io::stdout().lock();
            editor::console(mode, json, ports.endpoints(), &mut out)?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Editor {
            command:
                EditorCommand::Send(EditorSendCli {
                    message,
                    json,
                    ports,
                }),
        } => {
            let mut out = std::io::stdout().lock();
            editor::send_message(&message, json, ports.endpoints(), &mut out)?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Save {
            command:
                SaveCommand::Unpack(SaveUnpackCli {
                    save,
                    output,
                    force,
                    clean,
                    dry_run,
                    assets,
                    config,
                    json,
                }),
        } => {
            let map = if assets {
                let map = assets::load_asset_map(config.as_deref())?;
                if map.is_empty() {
                    return Err("--assets: no ttskit.toml with manifest entries that have local paths was found; run from the project directory or pass --config".into());
                }
                Some(map)
            } else {
                None
            };
            let mut out = std::io::stdout().lock();
            let safety = save::Safety {
                force,
                clean,
                dry_run,
            };
            save::unpack(&save, &output, safety, json, map.as_ref(), &mut out)?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Save {
            command:
                SaveCommand::Pack(SavePackCli {
                    dir,
                    output,
                    force,
                    dry_run,
                    keep_eol,
                    include,
                    config,
                    allow_stale,
                    json,
                }),
        } => {
            let mut out = std::io::stdout().lock();
            let mut err = std::io::stderr().lock();
            let safety = save::Safety {
                force,
                clean: false,
                dry_run,
            };
            let options = ttskit_core::PackOptions {
                includes: include,
                allow_stale,
                ..ttskit_core::PackOptions::new(keep_eol)
            };
            save::pack(
                &dir,
                &output,
                safety,
                &options,
                save::PackFormat {
                    config: config.as_deref(),
                    json,
                },
                &mut out,
                &mut err,
            )?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Cloud {
            command: CloudCommand::List(CloudListCli { folder, json }),
        } => {
            let backend = connect()?;
            let mut out = std::io::stdout().lock();
            cloud::list(backend.as_ref(), folder.as_deref(), json, &mut out)
                .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Cloud {
            command:
                CloudCommand::Restore(CloudRestoreCli {
                    backup,
                    yes,
                    backup_dir,
                }),
        } => {
            let dir = cloud::backup_dir(backup_dir).map_err(|e| e.to_string())?;
            let store = BackupStore::new(dir);
            let mut out = std::io::stdout().lock();
            match backup {
                None => cloud::list_backups(&store, &mut out).map_err(|e| e.to_string())?,
                Some(name) => {
                    // Validate the name before connecting to Steam.
                    let backup = store.get(&name).map_err(|e| e.to_string())?;
                    let mut backend = connect()?;
                    cloud::restore_backup(
                        backend.as_mut(),
                        &store,
                        &backup,
                        yes,
                        SystemTime::now(),
                        &mut out,
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Cloud {
            command:
                CloudCommand::Upload(CloudUploadCli {
                    file,
                    folder,
                    name,
                    dry_run,
                    yes,
                    json,
                    config,
                    backup_dir,
                }),
        } => {
            // Check everything local before connecting to Steam.
            let (name, data) =
                cloud::read_upload_file(&file, name.as_deref()).map_err(|e| e.to_string())?;
            let config = match config {
                Some(path) => Some(path),
                None => {
                    let default = PathBuf::from(ttskit_cloud::ProjectConfig::FILE_NAME);
                    default.exists().then_some(default)
                }
            };
            let manifest = match config {
                Some(config) => {
                    let project =
                        ttskit_cloud::ProjectConfig::load(&config).map_err(|e| e.to_string())?;
                    let config_dir = config_dir_of(&config);
                    let dir = project.resolve_local_dir(config_dir);
                    Some(cloud::ManifestTarget {
                        file: config_dir.join(ttskit_cloud::Manifest::FILE_NAME),
                        local_path: cloud::local_path_in(&dir, &file),
                    })
                }
                None => None,
            };
            let store = BackupStore::new(cloud::backup_dir(backup_dir).map_err(|e| e.to_string())?);
            let mut backend = connect()?;
            let req = cloud::UploadRequest {
                name: &name,
                data: &data,
                folder: folder.as_deref().unwrap_or_default(),
                mode: cloud::RunMode { dry_run, yes, json },
                manifest,
            };
            let mut out = std::io::stdout().lock();
            cloud::upload_file(
                backend.as_mut(),
                &store,
                &req,
                SystemTime::now(),
                &cloud::format_date(&jiff::Zoned::now()),
                &mut ask_on_terminal,
                &mut out,
            )
            .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Cloud {
            command:
                CloudCommand::Delete(CloudDeleteCli {
                    name,
                    folder,
                    yes,
                    json,
                    backup_dir,
                }),
        } => {
            let store = BackupStore::new(cloud::backup_dir(backup_dir).map_err(|e| e.to_string())?);
            let target = match (&name, &folder) {
                (Some(name), None) => ttskit_cloud::DeleteTarget::File(name),
                (None, Some(folder)) => ttskit_cloud::DeleteTarget::Folder(folder),
                _ => unreachable!("clap requires exactly one of name and --folder"),
            };
            let mut backend = connect()?;
            let mut out = std::io::stdout().lock();
            cloud::delete_files(
                backend.as_mut(),
                &store,
                target,
                yes,
                json,
                SystemTime::now(),
                &mut out,
            )
            .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Cloud {
            command:
                CloudCommand::Url(CloudUrlCli {
                    name,
                    share,
                    dry_run,
                    yes,
                    json,
                    backup_dir,
                }),
        } => {
            let store = BackupStore::new(cloud::backup_dir(backup_dir).map_err(|e| e.to_string())?);
            let mut backend = connect()?;
            let req = cloud::UrlRequest {
                query: &name,
                share,
                mode: cloud::RunMode { dry_run, yes, json },
            };
            let mut out = std::io::stdout().lock();
            cloud::show_url(
                backend.as_mut(),
                &store,
                &req,
                SystemTime::now(),
                &mut ask_on_terminal,
                &mut out,
            )
            .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Cloud {
            command:
                CloudCommand::Sync {
                    command:
                        SyncCommand::Push(SyncPushCli {
                            config,
                            prune,
                            dry_run,
                            yes,
                            json,
                            backup_dir,
                        }),
                },
        } => {
            // Do all the local work before connecting to Steam.
            let project = ttskit_cloud::ProjectConfig::load(&config).map_err(|e| e.to_string())?;
            let config_dir = config_dir_of(&config);
            let manifest_file = config_dir.join(ttskit_cloud::Manifest::FILE_NAME);
            let dir = project.resolve_local_dir(config_dir);
            let ignore = project.ignore_set();
            let scan =
                ttskit_cloud::scan_local_ignoring(&dir, &ignore).map_err(|e| e.to_string())?;
            let store = BackupStore::new(cloud::backup_dir(backup_dir).map_err(|e| e.to_string())?);
            let mut backend = connect()?;
            let mut out = std::io::stdout().lock();
            cloud::sync_push(
                backend.as_mut(),
                &store,
                &scan.files,
                &|rel| std::fs::read(dir.join(rel)),
                &project.cloud_folder,
                prune,
                &ignore,
                scan.ignored,
                Some(&manifest_file),
                cloud::RunMode { dry_run, yes, json },
                SystemTime::now(),
                &cloud::format_date(&jiff::Zoned::now()),
                &mut ask_on_terminal,
                &mut out,
            )
            .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Cloud {
            command:
                CloudCommand::Sync {
                    command:
                        SyncCommand::Pull(SyncPullCli {
                            config,
                            prune,
                            dry_run,
                            yes,
                            json,
                        }),
                },
        } => {
            // Do all the local work before connecting to Steam.
            let project = ttskit_cloud::ProjectConfig::load(&config).map_err(|e| e.to_string())?;
            let config_dir = config_dir_of(&config);
            let dir = project.resolve_local_dir(config_dir);
            // A directory that does not exist yet is an empty one; it is
            // created when a pull writes into it.
            let ignore = project.ignore_set();
            let (local, ignored) = if dir.exists() {
                let scan =
                    ttskit_cloud::scan_local_ignoring(&dir, &ignore).map_err(|e| e.to_string())?;
                (scan.files, scan.ignored)
            } else {
                (Vec::new(), 0)
            };
            let backend = connect()?;
            let mut local_dir = ttskit_cloud::FsDir::new(dir.clone());
            let mut out = std::io::stdout().lock();
            cloud::sync_pull(
                backend.as_ref(),
                &mut local_dir,
                &local,
                &project.cloud_folder,
                prune,
                &ignore,
                ignored,
                cloud::RunMode { dry_run, yes, json },
                &mut ask_on_terminal,
                &mut out,
            )
            .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Cloud {
            command:
                CloudCommand::Manifest(CloudManifestCli {
                    tsv,
                    json: _,
                    config,
                }),
        } => {
            ttskit_cloud::ProjectConfig::load(&config).map_err(|e| e.to_string())?;
            let file = config_dir_of(&config).join(ttskit_cloud::Manifest::FILE_NAME);
            let format = if tsv {
                cloud::ManifestFormat::Tsv
            } else {
                cloud::ManifestFormat::Json
            };
            let mut out = std::io::stdout().lock();
            cloud::show_manifest(&file, format, &mut out).map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Cloud {
            command:
                CloudCommand::ImportTsv(CloudImportTsvCli {
                    file,
                    config,
                    dry_run,
                    yes,
                    json,
                }),
        } => {
            // Do all the local work before connecting to Steam.
            let text = cloud::read_tsv_file(&file).map_err(|e| e.to_string())?;
            ttskit_cloud::ProjectConfig::load(&config).map_err(|e| e.to_string())?;
            let manifest_file = config_dir_of(&config).join(ttskit_cloud::Manifest::FILE_NAME);
            let backend = connect()?;
            let mut out = std::io::stdout().lock();
            cloud::import_tsv(
                backend.as_ref(),
                &text,
                &manifest_file,
                cloud::RunMode { dry_run, yes, json },
                &mut ask_on_terminal,
                &mut out,
            )
            .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Cloud {
            command:
                CloudCommand::Scan(CloudScanCli {
                    save,
                    adopt,
                    config,
                    dry_run,
                    yes,
                    json,
                    occurrences,
                }),
        } => {
            if !adopt {
                let mut out = std::io::stdout().lock();
                cloud::scan(&save, json, occurrences, &mut out).map_err(|e| e.to_string())?;
                return out.flush().map_err(|e| e.to_string().into());
            }
            // Do all the local work before connecting to Steam.
            let scan = cloud::read_scan(&save)
                .map_err(|e| e.to_string())?
                .with_occurrences(occurrences);
            ttskit_cloud::ProjectConfig::load(&config).map_err(|e| e.to_string())?;
            let manifest_file = config_dir_of(&config).join(ttskit_cloud::Manifest::FILE_NAME);
            let manifest = cloud::load_manifest(&manifest_file).map_err(|e| e.to_string())?;
            let backend = connect()?;
            let mut out = std::io::stdout().lock();
            cloud::scan_adopt(
                backend.as_ref(),
                &scan,
                manifest,
                &manifest_file,
                cloud::RunMode { dry_run, yes, json },
                &mut ask_on_terminal,
                &mut out,
            )
            .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Cloud {
            command: CloudCommand::Status(CloudStatusCli { .. }),
        }
        | Command::Image { .. }
        | Command::Save {
            command:
                SaveCommand::CheckUrls(_)
                | SaveCommand::FetchAssets(_)
                | SaveCommand::RewriteUrls(_)
                | SaveCommand::Rehost(_)
                | SaveCommand::Lint(_)
                | SaveCommand::Diff(_),
        } => unreachable!("handled by run"),
    }
}

/// Exit code of `editor` commands when the game does not accept a connection.
const EXIT_GAME_UNREACHABLE: u8 = 3;

/// Exit code of `cloud status`: 0 in sync, 2 otherwise.
fn status_exit_code(in_sync: bool) -> ExitCode {
    if in_sync {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}

/// Runs the command; the exit code is 0 unless the command says otherwise.
fn run(cli: Cli) -> Result<ExitCode, CliError> {
    match cli.command {
        Command::Cloud {
            command:
                CloudCommand::Status(CloudStatusCli {
                    dir,
                    folder,
                    json,
                    config,
                }),
        } => {
            // Scan the local side before connecting to Steam.
            let config = match config {
                Some(path) => Some(path),
                None => {
                    let default = PathBuf::from(ttskit_cloud::ProjectConfig::FILE_NAME);
                    default.exists().then_some(default)
                }
            };
            let ignore = match config {
                Some(config) => ttskit_cloud::ProjectConfig::load(&config)
                    .map_err(|e| e.to_string())?
                    .ignore_set(),
                None => ttskit_cloud::IgnoreSet::default(),
            };
            let scan =
                ttskit_cloud::scan_local_ignoring(&dir, &ignore).map_err(|e| e.to_string())?;
            let backend = connect()?;
            let mut out = std::io::stdout().lock();
            let in_sync = cloud::status(
                backend.as_ref(),
                &scan.files,
                folder.as_deref().unwrap_or_default(),
                &ignore,
                scan.ignored,
                &dir.display().to_string(),
                json,
                &mut out,
            )
            .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())?;
            Ok(status_exit_code(in_sync))
        }
        Command::Image {
            command: ImageCommand::Info(ImageInfoCli { inputs, json }),
        } => {
            let mut out = std::io::stdout().lock();
            let ok = image::info(&inputs, json, &mut out)?;
            out.flush().map_err(|e| e.to_string())?;
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Command::Image {
            command:
                ImageCommand::Check(ImageCheckCli {
                    inputs,
                    max_side,
                    max_bytes,
                    json,
                }),
        } => {
            let mut stdout = std::io::stdout().lock();
            let status = image::check(
                &image::CheckArgs {
                    inputs,
                    max_side,
                    max_bytes,
                    json,
                },
                &mut stdout,
            )?;
            stdout.flush().map_err(|e| e.to_string())?;
            Ok(match status {
                image::CheckStatus::Clean => ExitCode::SUCCESS,
                image::CheckStatus::Findings => ExitCode::from(2),
                image::CheckStatus::Errors => ExitCode::FAILURE,
            })
        }
        Command::Image {
            command:
                ImageCommand::Convert(ImageConvertCli {
                    inputs,
                    to,
                    quality,
                    background,
                    keep_metadata,
                    out,
                    out_dir,
                    yes,
                    dry_run,
                    json,
                }),
        } => {
            let mut stdout = std::io::stdout().lock();
            let ok = image::convert(
                &image::ConvertArgs {
                    inputs,
                    to,
                    quality,
                    background,
                    keep_metadata,
                    out,
                    out_dir,
                    yes,
                    dry_run,
                    json,
                },
                &mut stdout,
            )?;
            stdout.flush().map_err(|e| e.to_string())?;
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Command::Image {
            command:
                ImageCommand::Optimize(ImageOptimizeCli {
                    inputs,
                    level,
                    strip,
                    out,
                    out_dir,
                    in_place,
                    yes,
                    dry_run,
                    json,
                }),
        } => {
            let mut stdout = std::io::stdout().lock();
            let ok = image::optimize(
                &image::OptimizeArgs {
                    inputs,
                    level,
                    strip,
                    out,
                    out_dir,
                    in_place,
                    yes,
                    dry_run,
                    json,
                },
                &mut stdout,
            )?;
            stdout.flush().map_err(|e| e.to_string())?;
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Command::Image {
            command:
                ImageCommand::Resize(ImageResizeCli {
                    inputs,
                    max_side,
                    width,
                    height,
                    scale,
                    allow_enlarge,
                    filter,
                    quality,
                    out,
                    out_dir,
                    in_place,
                    yes,
                    dry_run,
                    json,
                }),
        } => {
            let mut stdout = std::io::stdout().lock();
            let ok = image::resize(
                &image::ResizeArgs {
                    inputs,
                    max_side,
                    width,
                    height,
                    scale,
                    allow_enlarge,
                    filter,
                    quality,
                    out,
                    out_dir,
                    in_place,
                    yes,
                    dry_run,
                    json,
                },
                &mut stdout,
            )?;
            stdout.flush().map_err(|e| e.to_string())?;
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Command::Image {
            command:
                ImageCommand::Quantize(ImageQuantizeCli {
                    inputs,
                    colors,
                    quality,
                    dither,
                    level,
                    strip,
                    engine,
                    pngquant,
                    out,
                    out_dir,
                    in_place,
                    yes,
                    dry_run,
                    json,
                }),
        } => {
            let mut stdout = std::io::stdout().lock();
            let ok = image::quantize(
                &image::QuantizeArgs {
                    inputs,
                    colors,
                    quality,
                    dither,
                    level,
                    strip,
                    engine,
                    pngquant,
                    out,
                    out_dir,
                    in_place,
                    yes,
                    dry_run,
                    json,
                },
                &mut stdout,
            )?;
            stdout.flush().map_err(|e| e.to_string())?;
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Command::Image {
            command:
                ImageCommand::Deck {
                    command:
                        DeckCommand::Build(DeckBuildCli {
                            cards,
                            list,
                            out_dir,
                            name,
                            to,
                            quality,
                            columns,
                            rows,
                            card_size,
                            fit,
                            hidden,
                            no_hidden_slot,
                            fill,
                            max_sheet_side,
                            yes,
                            dry_run,
                            json,
                        }),
                },
        } => {
            let mut stdout = std::io::stdout().lock();
            let ok = image::deck_build(
                &image::DeckBuildArgs {
                    cards,
                    list,
                    out_dir,
                    name,
                    to,
                    quality,
                    columns,
                    rows,
                    card_size,
                    fit,
                    hidden,
                    no_hidden_slot,
                    fill,
                    max_sheet_side,
                    yes,
                    dry_run,
                    json,
                },
                &mut stdout,
            )?;
            stdout.flush().map_err(|e| e.to_string())?;
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Command::Image {
            command:
                ImageCommand::Deck {
                    command:
                        DeckCommand::Split(DeckSplitCli {
                            sheet,
                            out_dir,
                            name,
                            to,
                            quality,
                            fill,
                            columns,
                            rows,
                            description,
                            save,
                            deck_id,
                            count,
                            no_hidden_slot,
                            skip_hidden,
                            yes,
                            dry_run,
                            json,
                        }),
                },
        } => {
            let mut stdout = std::io::stdout().lock();
            let ok = image::deck_split(
                &image::DeckSplitArgs {
                    sheet,
                    out_dir,
                    name,
                    to,
                    quality,
                    fill,
                    columns,
                    rows,
                    description,
                    save,
                    deck_id,
                    count,
                    no_hidden_slot,
                    skip_hidden,
                    yes,
                    dry_run,
                    json,
                },
                &mut stdout,
            )?;
            stdout.flush().map_err(|e| e.to_string())?;
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Command::Image {
            command:
                ImageCommand::Deck {
                    command:
                        DeckCommand::Object(DeckObjectCli {
                            description,
                            out,
                            face_urls,
                            back_urls,
                            unique_backs,
                            config,
                            meta,
                            nicknames_from_files,
                            yes,
                            dry_run,
                            json,
                        }),
                },
        } => {
            let mut stdout = std::io::stdout().lock();
            let ok = image::deck_object(
                &image::DeckObjectArgs {
                    description,
                    out,
                    face_urls,
                    back_urls,
                    unique_backs,
                    config,
                    meta,
                    nicknames_from_files,
                    yes,
                    dry_run,
                    json,
                },
                &mut stdout,
            )?;
            stdout.flush().map_err(|e| e.to_string())?;
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Command::Image {
            command:
                ImageCommand::Build(ImageBuildCli {
                    config,
                    dry_run,
                    prune,
                    yes,
                    json,
                }),
        } => run_image_build(image::BuildArgs {
            config,
            dry_run,
            prune,
            yes,
            json,
        }),
        Command::Save {
            command:
                SaveCommand::CheckUrls(SaveCheckUrlsCli {
                    save,
                    verify_content,
                    host,
                    skip_host,
                    concurrency,
                    per_host,
                    timeout,
                    retries,
                    json,
                }),
        } => {
            let client = http::HttpClient::new(std::time::Duration::from_secs(u64::from(timeout)))?;
            let config = ttskit_core::CheckConfig {
                concurrency: concurrency as usize,
                per_host: per_host as usize,
                retries,
                verify_content,
                only_hosts: host,
                skip_hosts: skip_host,
                ..ttskit_core::CheckConfig::default()
            };
            let mut out = std::io::stdout().lock();
            let status = check_urls::check_urls(&save, &client, &config, json, &mut out)?;
            out.flush().map_err(|e| e.to_string())?;
            Ok(match status {
                check_urls::CheckStatus::Clean => ExitCode::SUCCESS,
                check_urls::CheckStatus::Problems => ExitCode::from(2),
            })
        }
        Command::Save {
            command: SaveCommand::FetchAssets(args),
        } => run_fetch_assets(args),
        Command::Save {
            command: SaveCommand::RewriteUrls(args),
        } => run_rewrite_urls(args),
        Command::Save {
            command: SaveCommand::Rehost(args),
        } => run_rehost(args),
        Command::Save {
            command: SaveCommand::Lint(args),
        } => run_lint(args),
        Command::Save {
            command: SaveCommand::Diff(args),
        } => run_diff(args),
        command => run_simple(command).map(|()| ExitCode::SUCCESS),
    }
}

#[inline(never)]
fn run_diff(args: SaveDiffCli) -> Result<ExitCode, CliError> {
    let options = diff::Options {
        old: &args.old,
        new: &args.new,
        tolerance: diff::parse_tolerance(&args.tolerance)?,
        scripts: args.scripts,
        verbose: args.verbose,
        json: args.json,
    };
    let mut out = std::io::stdout().lock();
    let status = diff::diff(&options, &mut out)?;
    out.flush().map_err(|e| e.to_string())?;
    Ok(match status {
        diff::Status::Equal => ExitCode::SUCCESS,
        diff::Status::Different => ExitCode::from(2),
    })
}

#[inline(never)]
fn run_lint(args: SaveLintCli) -> Result<ExitCode, CliError> {
    let mut out = std::io::stdout().lock();
    if args.list_rules {
        lint::list_rules(&mut out)?;
        out.flush().map_err(|e| e.to_string())?;
        return Ok(ExitCode::SUCCESS);
    }
    let save = args.save.ok_or("no save file given")?;
    let options = lint::Options {
        save: &save,
        config: args.config.as_deref(),
        allow: &args.allow,
        warn: &args.warn,
        deny: &args.deny,
        level: lint::parse_level(&args.level)?,
        json: args.json,
    };
    let status = lint::lint(&options, &mut out)?;
    out.flush().map_err(|e| e.to_string())?;
    Ok(match status {
        lint::Status::Clean => ExitCode::SUCCESS,
        lint::Status::Findings => ExitCode::from(2),
    })
}

#[inline(never)]
fn run_rewrite_urls(args: SaveRewriteUrlsCli) -> Result<ExitCode, CliError> {
    let options = rewrite_urls::Options {
        save: &args.save,
        map: args.map.as_deref(),
        manifest: args.from_manifest.as_deref(),
        normalize_steam_hosts: args.normalize_steam_hosts,
        in_scripts: args.in_scripts,
        out: args.out.as_deref(),
        in_place: args.in_place,
        yes: args.yes,
        dry_run: args.dry_run,
        json: args.json,
    };
    let mut out = std::io::stdout().lock();
    rewrite_urls::rewrite_urls(&options, &mut out)?;
    out.flush().map_err(|e| e.to_string())?;
    Ok(ExitCode::SUCCESS)
}

#[inline(never)]
fn run_rehost(args: SaveRehostCli) -> Result<ExitCode, CliError> {
    let client =
        http::HttpClient::for_downloads(std::time::Duration::from_secs(u64::from(args.timeout)))?;
    let prepared = rehost::prepare(rehost::Options {
        save: args.save,
        config: args.config,
        folder: args.folder,
        hosts: args.host,
        skip_hosts: args.skip_host,
        out: args.out,
        in_place: args.in_place,
        in_scripts: args.in_scripts,
        yes: args.yes,
        dry_run: args.dry_run,
        fetch: ttskit_core::FetchConfig {
            concurrency: args.concurrency as usize,
            per_host: args.per_host as usize,
            retries: args.retries,
            max_size: args.max_size,
            ..ttskit_core::FetchConfig::default()
        },
    })?;
    let store = BackupStore::new(cloud::backup_dir(args.backup_dir).map_err(|e| e.to_string())?);
    let mut backend = connect()?;
    let mut out = std::io::stdout().lock();
    let status = rehost::run(
        &prepared,
        backend.as_mut(),
        &client,
        &client,
        &store,
        SystemTime::now(),
        &cloud::format_date(&jiff::Zoned::now()),
        &mut ask_on_terminal,
        &mut out,
    )?;
    out.flush().map_err(|e| e.to_string())?;
    Ok(match status {
        rehost::Status::Clean => ExitCode::SUCCESS,
        rehost::Status::Problems => ExitCode::from(2),
    })
}

#[inline(never)]
fn run_fetch_assets(args: SaveFetchAssetsCli) -> Result<ExitCode, CliError> {
    let client =
        http::HttpClient::for_downloads(std::time::Duration::from_secs(u64::from(args.timeout)))?;
    let options = fetch_assets::Options {
        save: &args.save,
        out: &args.out,
        dry_run: args.dry_run,
        json: args.json,
        cache_dir: args.cache_dir.as_deref(),
        config: ttskit_core::FetchConfig {
            concurrency: args.concurrency as usize,
            per_host: args.per_host as usize,
            retries: args.retries,
            max_size: args.max_size,
            only_hosts: args.host,
            skip_hosts: args.skip_host,
            verify: args.verify,
            offline: args.offline,
            ..ttskit_core::FetchConfig::default()
        },
    };
    let mut out = std::io::stdout().lock();
    let status = fetch_assets::fetch_assets(&options, &client, &mut out)?;
    out.flush().map_err(|e| e.to_string())?;
    Ok(match status {
        fetch_assets::FetchStatus::Clean => ExitCode::SUCCESS,
        fetch_assets::FetchStatus::Problems => ExitCode::from(2),
    })
}

/// Kept out of `run`, whose stack frame in a debug build is already close to
/// the 1 MiB main-thread stack of Windows.
#[inline(never)]
fn run_image_build(args: image::BuildArgs) -> Result<ExitCode, CliError> {
    let mut stdout = std::io::stdout().lock();
    let ok = image::build(&args, &mut stdout)?;
    stdout.flush().map_err(|e| e.to_string())?;
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            if error.game_unreachable {
                ExitCode::from(EXIT_GAME_UNREACHABLE)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(
            ["ttskit", "cloud", "delete"]
                .into_iter()
                .chain(args.iter().copied()),
        )
    }

    #[test]
    fn delete_needs_exactly_one_of_name_and_folder() {
        assert!(parse(&["a.png"]).is_ok());
        assert!(parse(&["--folder", "Decks", "--yes"]).is_ok());
        assert!(parse(&[]).is_err());
        assert!(parse(&["--yes"]).is_err());
        assert!(parse(&["a.png", "--folder", "Decks"]).is_err());
    }

    #[test]
    fn scan_parses() {
        let cli = Cli::try_parse_from(["ttskit", "cloud", "scan", "s.json", "--json"]).unwrap();
        let Command::Cloud {
            command:
                CloudCommand::Scan(CloudScanCli {
                    save,
                    adopt,
                    config,
                    dry_run,
                    yes,
                    json,
                    occurrences: _,
                }),
        } = cli.command
        else {
            panic!("expected scan");
        };
        assert_eq!(save, PathBuf::from("s.json"));
        assert!(json);
        assert!(!adopt && !dry_run && !yes);
        assert_eq!(config, PathBuf::from("ttskit.toml"));
        assert!(Cli::try_parse_from(["ttskit", "cloud", "scan"]).is_err());
    }

    #[test]
    fn scan_adopt_options() {
        let scan = |extra: &[&str]| {
            Cli::try_parse_from(
                ["ttskit", "cloud", "scan", "s.json"]
                    .into_iter()
                    .chain(extra.iter().copied()),
            )
        };
        let Command::Cloud {
            command:
                CloudCommand::Scan(CloudScanCli {
                    adopt,
                    config,
                    dry_run,
                    yes,
                    ..
                }),
        } = scan(&["--adopt", "--config", "p/x.toml", "--yes"])
            .unwrap()
            .command
        else {
            panic!("expected scan");
        };
        assert!(adopt && yes && !dry_run);
        assert_eq!(config, PathBuf::from("p/x.toml"));
        assert!(scan(&["--adopt"]).is_ok());
        assert!(scan(&["--adopt", "--dry-run", "--json"]).is_ok());
        assert!(scan(&["--adopt", "--dry-run", "--yes"]).is_err());
        assert!(scan(&["--dry-run"]).is_err());
        assert!(scan(&["--yes"]).is_err());
        assert!(scan(&["--config", "x.toml"]).is_err());
    }

    fn parse_push(args: &[&str]) -> Result<SyncCommand, clap::Error> {
        let cli = Cli::try_parse_from(
            ["ttskit", "cloud", "sync", "push"]
                .into_iter()
                .chain(args.iter().copied()),
        )?;
        match cli.command {
            Command::Cloud {
                command: CloudCommand::Sync { command },
            } => Ok(command),
            _ => unreachable!("parsed sync push"),
        }
    }

    #[test]
    fn sync_push_defaults() {
        let SyncCommand::Push(SyncPushCli {
            config,
            prune,
            dry_run,
            yes,
            json,
            ..
        }) = parse_push(&[]).unwrap()
        else {
            panic!("expected push");
        };
        assert_eq!(config, PathBuf::from("ttskit.toml"));
        assert!(!prune && !dry_run && !yes && !json);
    }

    #[test]
    fn sync_push_options() {
        let SyncCommand::Push(SyncPushCli {
            config, prune, yes, ..
        }) = parse_push(&["--config", "proj/my.toml", "--prune", "--yes"]).unwrap()
        else {
            panic!("expected push");
        };
        assert_eq!(config, PathBuf::from("proj/my.toml"));
        assert!(prune && yes);
        assert!(parse_push(&["--dry-run", "--json"]).is_ok());
    }

    #[test]
    fn sync_push_rejects_dry_run_with_yes() {
        assert!(parse_push(&["--dry-run", "--yes"]).is_err());
    }

    fn parse_pull(args: &[&str]) -> Result<SyncCommand, clap::Error> {
        let cli = Cli::try_parse_from(
            ["ttskit", "cloud", "sync", "pull"]
                .into_iter()
                .chain(args.iter().copied()),
        )?;
        match cli.command {
            Command::Cloud {
                command: CloudCommand::Sync { command },
            } => Ok(command),
            _ => unreachable!("parsed sync pull"),
        }
    }

    #[test]
    fn sync_pull_defaults() {
        let SyncCommand::Pull(SyncPullCli {
            config,
            prune,
            dry_run,
            yes,
            json,
        }) = parse_pull(&[]).unwrap()
        else {
            panic!("expected pull");
        };
        assert_eq!(config, PathBuf::from("ttskit.toml"));
        assert!(!prune && !dry_run && !yes && !json);
    }

    #[test]
    fn sync_pull_options() {
        let SyncCommand::Pull(SyncPullCli {
            config, prune, yes, ..
        }) = parse_pull(&["--config", "proj/my.toml", "--prune", "--yes"]).unwrap()
        else {
            panic!("expected pull");
        };
        assert_eq!(config, PathBuf::from("proj/my.toml"));
        assert!(prune && yes);
        assert!(parse_pull(&["--dry-run", "--json"]).is_ok());
    }

    #[test]
    fn sync_pull_rejects_dry_run_with_yes() {
        assert!(parse_pull(&["--dry-run", "--yes"]).is_err());
    }

    #[test]
    fn sync_pull_has_no_backup_dir() {
        assert!(parse_pull(&["--backup-dir", "x"]).is_err());
    }

    fn parse_status(args: &[&str]) -> Result<Option<PathBuf>, clap::Error> {
        let cli = Cli::try_parse_from(
            ["ttskit", "cloud", "status", "assets"]
                .into_iter()
                .chain(args.iter().copied()),
        )?;
        match cli.command {
            Command::Cloud {
                command: CloudCommand::Status(CloudStatusCli { config, .. }),
            } => Ok(config),
            _ => unreachable!("parsed status"),
        }
    }

    #[test]
    fn status_config_is_optional() {
        assert_eq!(parse_status(&[]).unwrap(), None);
        assert_eq!(
            parse_status(&["--config", "proj/my.toml"]).unwrap(),
            Some(PathBuf::from("proj/my.toml"))
        );
    }

    #[test]
    fn status_exit_codes() {
        assert_eq!(status_exit_code(true), ExitCode::SUCCESS);
        assert_eq!(status_exit_code(false), ExitCode::from(2));
    }

    fn parse_build(args: &[&str]) -> Result<(PathBuf, bool, bool, bool, bool), clap::Error> {
        let cli = Cli::try_parse_from(
            ["ttskit", "image", "build"]
                .into_iter()
                .chain(args.iter().copied()),
        )?;
        match cli.command {
            Command::Image {
                command:
                    ImageCommand::Build(ImageBuildCli {
                        config,
                        dry_run,
                        prune,
                        yes,
                        json,
                    }),
            } => Ok((config, dry_run, prune, yes, json)),
            _ => unreachable!("parsed image build"),
        }
    }

    #[test]
    fn image_build_defaults() {
        let (config, dry_run, prune, yes, json) = parse_build(&[]).unwrap();
        assert_eq!(
            config,
            PathBuf::from(ttskit_cloud::ProjectConfig::FILE_NAME)
        );
        assert!(!dry_run && !prune && !yes && !json);
    }

    #[test]
    fn image_build_flags() {
        let (config, dry_run, prune, yes, json) = parse_build(&[
            "--config",
            "proj/my.toml",
            "--dry-run",
            "--prune",
            "--yes",
            "--json",
        ])
        .unwrap();
        assert_eq!(config, PathBuf::from("proj/my.toml"));
        assert!(dry_run && prune && yes && json);
        assert!(parse_build(&["--bogus"]).is_err());
    }
}
