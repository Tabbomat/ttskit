mod cloud;
mod editor;
mod image;
mod mcp;
mod save;
mod watch;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use editor::CliError;
use std::time::SystemTime;

use clap::{Parser, Subcommand};
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
    /// Inspect, check, convert, resize, optimize and quantize image files, and build deck sheets from card images (local only; needs a build with the `image` feature; unofficial)
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
    Mcp {
        /// Project directory; save_and_play may only read paths inside it (default: the current directory)
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
        /// Keep at most this many events (print, error, custom, saved, loaded) for get_console; the buffer also holds at most 4 MiB of text
        #[arg(long, value_name = "EVENTS", default_value_t = 1000, value_parser = clap::value_parser!(u32).range(1..))]
        console_buffer: u32,
        #[command(flatten)]
        ports: EditorPorts,
    },
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
    Watch {
        /// The directory written by `save unpack`
        dir: PathBuf,
        /// Inline `require("Name")` and `<Include src="Name"/>` from this directory
        /// and watch it too; repeatable, searched in order
        #[arg(long, value_name = "LIB_DIR")]
        include: Vec<PathBuf>,
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
    },
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
    GetScripts {
        /// Print the full scripts and UI as JSON instead of a summary
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        ports: EditorPorts,
    },
    /// Send the scripts and UI of an unpacked save to the game and reload it (Save & Play)
    ///
    /// Reloads the whole game and discards unsaved changes in it. Only objects
    /// whose script or UI differ from the game are sent, each with both fields
    /// in full. Without --yes the changes are shown and nothing is sent.
    SaveAndPlay {
        /// A directory written by `save unpack`; objects are matched by GUID
        dir: PathBuf,
        /// Inline `require("Name")` and `<Include src="Name"/>` from this
        /// directory, as `save pack --include` does; repeatable
        #[arg(long, value_name = "LIB_DIR")]
        include: Vec<PathBuf>,
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
    },
    /// Run Lua in the game and print what it reports
    ///
    /// Runs in the context of an object (default: Global). The object must
    /// already have a script, or the game reports an error. The game was not
    /// observed to answer `return` statements, so use `print(...)` for output;
    /// a returned value is shown if the game sends one. Output printed by the
    /// game's own scripts in the meantime is included. A Lua error makes the
    /// command fail.
    Exec {
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
    },
    /// Print the game's `print` output and Lua errors, stamped on arrival
    ///
    /// Without --follow, listens until --quiet-ms pass without a message (or
    /// --timeout seconds in total) and exits 0; with --follow, runs until
    /// Ctrl+C. Only messages that arrive while this runs are seen, and it holds
    /// the editor port, so other `editor` commands cannot run meanwhile. The
    /// game sends no timestamp and no origin for `print`; a Lua error shows the
    /// GUID of the failing object. Lua errors are output and do not change the
    /// exit status. `--json` prints one JSON object per line.
    Console {
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
    },
    /// Send a custom message to the game's `onExternalMessage` handler
    ///
    /// The message must be a JSON object; the game ignores anything else.
    /// The game does not acknowledge it, so success only means it was sent.
    Send {
        /// A JSON object, for example '{"action":"ping"}'
        message: String,
        /// Print machine-readable JSON instead of text
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        ports: EditorPorts,
    },
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
    Info {
        /// Image files or directories
        #[arg(required = true, value_name = "PATH")]
        inputs: Vec<PathBuf>,
        /// Print one JSON object with all results
        #[arg(long)]
        json: bool,
    },
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
    Check {
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
    },
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
    Convert {
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
    },
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
    Optimize {
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
    },
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
    #[command(group(clap::ArgGroup::new("size").required(true).multiple(false).args(["max_side", "width", "height", "scale"])))]
    Resize {
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
    },
    /// Build the sheets of custom decks
    ///
    /// A custom deck is one or more grid sheets of card images. `deck build`
    /// assembles single card images into such sheets.
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
    Quantize {
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
    },
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
    Build {
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
    },
}

#[derive(Subcommand)]
enum SaveCommand {
    /// Unpack a save file into a directory of scripts, UI, state and objects
    Unpack {
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
        /// Print machine-readable JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Pack an unpacked directory back into a save file
    Pack {
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
        /// Print machine-readable JSON instead of text
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum CloudCommand {
    /// List the folders and files in the Steam Cloud (read-only)
    List {
        /// Only show this folder and its subfolders (separator `/`)
        #[arg(long, value_name = "FOLDER")]
        folder: Option<String>,
        /// Print machine-readable JSON instead of a tree
        #[arg(long)]
        json: bool,
    },
    /// Restore the cloud index from a local backup (dry run unless --yes)
    Restore {
        /// Backup to restore; omit to list the backups
        backup: Option<String>,
        /// Restore for real; without it only the plan is printed
        #[arg(long)]
        yes: bool,
        /// Directory holding the backups [default: <local app data>/ttskit/cloud-backups]
        #[arg(long, env = "TTSKIT_BACKUP_DIR", value_name = "DIR")]
        backup_dir: Option<PathBuf>,
    },
    /// Upload a file to the Steam Cloud, share it and print its URL
    Upload {
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
    },
    /// Delete a file or folder from the Steam Cloud (dry run unless --yes)
    #[command(group(clap::ArgGroup::new("what").required(true).args(["name", "folder"])))]
    Delete {
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
    },
    /// Compare a local directory with a cloud folder (read-only)
    Status {
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
    },
    /// Make a cloud folder match a local directory (unofficial)
    Sync {
        #[command(subcommand)]
        command: SyncCommand,
    },
    /// Print the project manifest (local only, no Steam needed)
    Manifest {
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
    },
    /// Add a `filename<TAB>URL` list of already shared files to the manifest
    ImportTsv {
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
    },
    /// List the asset URLs of a save file, grouped by host (local only, no Steam needed)
    Scan {
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
    },
    /// Print the download URL of a cloud file
    Url {
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
    },
}

#[derive(Subcommand)]
enum SyncCommand {
    /// Upload the local directory to the cloud folder named in the project config
    Push {
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
    },
    /// Download the cloud folder named in the project config into the local directory
    Pull {
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
    },
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
        Command::Mcp {
            root,
            console_buffer,
            ports,
        } => mcp::run(root.as_deref(), ports.endpoints(), console_buffer as usize),
        Command::Watch {
            dir,
            include,
            once,
            pack_to,
            debounce_ms,
            quiet,
            ports,
        } => {
            let options = watch::WatchOptions {
                dir,
                includes: include,
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
            command: EditorCommand::GetScripts { json, ports },
        } => {
            let mut out = std::io::stdout().lock();
            editor::get_scripts(ports.endpoints(), json, &mut out)?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Editor {
            command:
                EditorCommand::SaveAndPlay {
                    dir,
                    include,
                    dry_run,
                    yes,
                    allow_delete,
                    json,
                    ports,
                },
        } => {
            // Read and pack the tree before touching the game.
            let desired = editor::desired_from_tree(&dir, include)?;
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
                EditorCommand::Exec {
                    code,
                    guid,
                    quiet,
                    json,
                    ports,
                },
        } => {
            let mut out = std::io::stdout().lock();
            let wait = std::time::Duration::from_millis(quiet.quiet_ms);
            editor::exec(&code, &guid, wait, json, ports.endpoints(), &mut out)?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Editor {
            command:
                EditorCommand::Console {
                    follow,
                    json,
                    quiet,
                    ports,
                },
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
                EditorCommand::Send {
                    message,
                    json,
                    ports,
                },
        } => {
            let mut out = std::io::stdout().lock();
            editor::send_message(&message, json, ports.endpoints(), &mut out)?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Save {
            command:
                SaveCommand::Unpack {
                    save,
                    output,
                    force,
                    clean,
                    dry_run,
                    json,
                },
        } => {
            let mut out = std::io::stdout().lock();
            let safety = save::Safety {
                force,
                clean,
                dry_run,
            };
            save::unpack(&save, &output, safety, json, &mut out)?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Save {
            command:
                SaveCommand::Pack {
                    dir,
                    output,
                    force,
                    dry_run,
                    keep_eol,
                    include,
                    json,
                },
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
                ..ttskit_core::PackOptions::new(keep_eol)
            };
            save::pack(&dir, &output, safety, &options, json, &mut out, &mut err)?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Cloud {
            command: CloudCommand::List { folder, json },
        } => {
            let backend = connect()?;
            let mut out = std::io::stdout().lock();
            cloud::list(backend.as_ref(), folder.as_deref(), json, &mut out)
                .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string().into())
        }
        Command::Cloud {
            command:
                CloudCommand::Restore {
                    backup,
                    yes,
                    backup_dir,
                },
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
                CloudCommand::Upload {
                    file,
                    folder,
                    name,
                    dry_run,
                    yes,
                    json,
                    config,
                    backup_dir,
                },
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
                CloudCommand::Delete {
                    name,
                    folder,
                    yes,
                    json,
                    backup_dir,
                },
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
                CloudCommand::Url {
                    name,
                    share,
                    dry_run,
                    yes,
                    json,
                    backup_dir,
                },
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
                        SyncCommand::Push {
                            config,
                            prune,
                            dry_run,
                            yes,
                            json,
                            backup_dir,
                        },
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
                        SyncCommand::Pull {
                            config,
                            prune,
                            dry_run,
                            yes,
                            json,
                        },
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
                CloudCommand::Manifest {
                    tsv,
                    json: _,
                    config,
                },
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
                CloudCommand::ImportTsv {
                    file,
                    config,
                    dry_run,
                    yes,
                    json,
                },
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
                CloudCommand::Scan {
                    save,
                    adopt,
                    config,
                    dry_run,
                    yes,
                    json,
                    occurrences,
                },
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
            command: CloudCommand::Status { .. },
        }
        | Command::Image { .. } => unreachable!("handled by run"),
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
                CloudCommand::Status {
                    dir,
                    folder,
                    json,
                    config,
                },
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
            command: ImageCommand::Info { inputs, json },
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
                ImageCommand::Check {
                    inputs,
                    max_side,
                    max_bytes,
                    json,
                },
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
                ImageCommand::Convert {
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
                ImageCommand::Optimize {
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
                ImageCommand::Resize {
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
                ImageCommand::Quantize {
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
                        DeckCommand::Build {
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
        command => run_simple(command).map(|()| ExitCode::SUCCESS),
    }
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
                CloudCommand::Scan {
                    save,
                    adopt,
                    config,
                    dry_run,
                    yes,
                    json,
                    occurrences: _,
                },
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
                CloudCommand::Scan {
                    adopt,
                    config,
                    dry_run,
                    yes,
                    ..
                },
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
        let SyncCommand::Push {
            config,
            prune,
            dry_run,
            yes,
            json,
            ..
        } = parse_push(&[]).unwrap()
        else {
            panic!("expected push");
        };
        assert_eq!(config, PathBuf::from("ttskit.toml"));
        assert!(!prune && !dry_run && !yes && !json);
    }

    #[test]
    fn sync_push_options() {
        let SyncCommand::Push {
            config, prune, yes, ..
        } = parse_push(&["--config", "proj/my.toml", "--prune", "--yes"]).unwrap()
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
        let SyncCommand::Pull {
            config,
            prune,
            dry_run,
            yes,
            json,
        } = parse_pull(&[]).unwrap()
        else {
            panic!("expected pull");
        };
        assert_eq!(config, PathBuf::from("ttskit.toml"));
        assert!(!prune && !dry_run && !yes && !json);
    }

    #[test]
    fn sync_pull_options() {
        let SyncCommand::Pull {
            config, prune, yes, ..
        } = parse_pull(&["--config", "proj/my.toml", "--prune", "--yes"]).unwrap()
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
                command: CloudCommand::Status { config, .. },
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
}
