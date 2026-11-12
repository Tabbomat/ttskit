mod cloud;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
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
        /// Also replace changed files and delete cloud files that are not local
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
        /// Also move local files that are not in the cloud folder to the recycle bin
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

fn run_simple(command: Command) -> Result<(), String> {
    match command {
        Command::Cloud {
            command: CloudCommand::List { folder, json },
        } => {
            let backend = connect()?;
            let mut out = std::io::stdout().lock();
            cloud::list(backend.as_ref(), folder.as_deref(), json, &mut out)
                .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())
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
            out.flush().map_err(|e| e.to_string())
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
            out.flush().map_err(|e| e.to_string())
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
            out.flush().map_err(|e| e.to_string())
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
            out.flush().map_err(|e| e.to_string())
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
            let local = ttskit_cloud::scan_local(&dir).map_err(|e| e.to_string())?;
            let store = BackupStore::new(cloud::backup_dir(backup_dir).map_err(|e| e.to_string())?);
            let mut backend = connect()?;
            let mut out = std::io::stdout().lock();
            cloud::sync_push(
                backend.as_mut(),
                &store,
                &local,
                &|rel| std::fs::read(dir.join(rel)),
                &project.cloud_folder,
                prune,
                Some(&manifest_file),
                cloud::RunMode { dry_run, yes, json },
                SystemTime::now(),
                &cloud::format_date(&jiff::Zoned::now()),
                &mut ask_on_terminal,
                &mut out,
            )
            .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())
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
            let local = if dir.exists() {
                ttskit_cloud::scan_local(&dir).map_err(|e| e.to_string())?
            } else {
                Vec::new()
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
                cloud::RunMode { dry_run, yes, json },
                &mut ask_on_terminal,
                &mut out,
            )
            .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())
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
            out.flush().map_err(|e| e.to_string())
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
            out.flush().map_err(|e| e.to_string())
        }
        Command::Cloud {
            command: CloudCommand::Status { .. },
        } => unreachable!("handled by run"),
    }
}

/// Exit code of `cloud status`: 0 in sync, 2 otherwise.
fn status_exit_code(in_sync: bool) -> ExitCode {
    if in_sync {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}

/// Runs the command; the exit code is 0 unless the command says otherwise.
fn run(cli: Cli) -> Result<ExitCode, String> {
    match cli.command {
        Command::Cloud {
            command: CloudCommand::Status { dir, folder, json },
        } => {
            // Scan the local side before connecting to Steam.
            let local = ttskit_cloud::scan_local(&dir).map_err(|e| e.to_string())?;
            let backend = connect()?;
            let mut out = std::io::stdout().lock();
            let in_sync = cloud::status(
                backend.as_ref(),
                &local,
                folder.as_deref().unwrap_or_default(),
                &dir.display().to_string(),
                json,
                &mut out,
            )
            .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())?;
            Ok(status_exit_code(in_sync))
        }
        command => run_simple(command).map(|()| ExitCode::SUCCESS),
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
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

    #[test]
    fn status_exit_codes() {
        assert_eq!(status_exit_code(true), ExitCode::SUCCESS);
        assert_eq!(status_exit_code(false), ExitCode::from(2));
    }
}
