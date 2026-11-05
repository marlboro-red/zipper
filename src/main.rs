use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::{io::IsTerminal, path::PathBuf};
use zeroize::Zeroizing;
use zipper::Reader;

#[derive(Parser)]
#[command(
    version,
    about = "Zipper — fast, safe extraction of ZIP, tar and 7z archives"
)]
struct Cli {
    /// Maximum worker threads (defaults to available CPUs; tiny ZIPs may use fewer; tar uses 4 writers).
    #[arg(long, global = true)]
    threads: Option<usize>,
    /// Reader for ZIP, tar and other foreign archives. Native readers restore files, directories, modes, times and safe links.
    #[arg(long, global = true, value_enum, default_value_t = ZipEngine::Auto)]
    zip_engine: ZipEngine,
    /// Shared native-fast ZIP entry-buffer MiB (128 on ARM64 macOS, 64 elsewhere; 0 streams).
    /// Metadata, worker state and verification archive cache are additional.
    #[arg(long, global = true, value_parser = clap::value_parser!(u32).range(0..=4096))]
    zip_buffer_mib: Option<u32>,
    /// Password for encrypted ZIP entries. Other users can see it in the process list.
    /// Without it, Zipper asks on Unix when a selected entry is encrypted and standard
    /// input is a terminal, and fails otherwise.
    #[arg(
        long,
        global = true,
        value_name = "PASSWORD",
        allow_hyphen_values = true
    )]
    password: Option<String>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ZipEngine {
    /// Native for ZIP, tar, unrecognized files and 7z it decodes; 7-Zip for other recognized formats.
    Auto,
    /// 7-Zip for every archive.
    Backend,
    Native,
    /// Native ZIP with a shared entry-buffer budget and streaming fallback.
    NativeFast,
}
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ExistingArg {
    /// Refuse an existing destination.
    Refuse,
    /// Replace each file of the same name (7-Zip -aoa).
    Overwrite,
    /// Keep each existing file and leave the archive's copy out (7-Zip -aos).
    Skip,
    /// Write the archive's copy as name_1.ext (7-Zip -aou).
    RenameNew,
    /// Rename the existing file to name_1.ext first (7-Zip -aot).
    RenameExisting,
}
impl From<ExistingArg> for zipper::Existing {
    fn from(existing: ExistingArg) -> Self {
        match existing {
            ExistingArg::Refuse => zipper::Existing::Refuse,
            ExistingArg::Overwrite => zipper::Existing::Overwrite,
            ExistingArg::Skip => zipper::Existing::Skip,
            ExistingArg::RenameNew => zipper::Existing::RenameNew,
            ExistingArg::RenameExisting => zipper::Existing::RenameExisting,
        }
    }
}
#[derive(Subcommand)]
enum Command {
    /// Extract into a new directory, or into an existing one with --overwrite.
    Extract {
        /// The archive, or `-` to read it from standard input.
        archive: PathBuf,
        #[arg(short, long, required_unless_present = "stdout")]
        output: Option<PathBuf>,
        /// Write the one selected file to standard output instead of a folder (7-Zip -so).
        #[arg(long, conflicts_with_all = ["output", "flat", "overwrite", "existing"])]
        stdout: bool,
        /// Extract into an existing directory, replacing files of the same name.
        #[arg(long, conflicts_with = "existing")]
        overwrite: bool,
        /// What to do with paths the existing destination already holds.
        #[arg(long, value_enum)]
        existing: Option<ExistingArg>,
        /// Extract an exact archive path or folder subtree (repeatable).
        #[arg(long = "path")]
        paths: Vec<String>,
        /// Extract entries matching a wildcard pattern (repeatable). `*` and `?`
        /// stay in one name, `**` crosses folders, and a pattern without `/`
        /// matches a name at any depth.
        #[arg(long)]
        include: Vec<String>,
        /// Leave out entries matching a wildcard pattern (repeatable).
        #[arg(long)]
        exclude: Vec<String>,
        /// Extract every file into the output folder itself, without its
        /// folders (7-Zip e). Files that share a name follow --existing.
        #[arg(long)]
        flat: bool,
        /// Extract only this archive folder, with its contents at the output
        /// root: with `a`, `a/b/c.txt` lands at `<output>/b/c.txt`.
        #[arg(long, value_name = "PATH", conflicts_with = "strip_components")]
        strip_prefix: Option<String>,
        /// Drop this many leading folders from every path, as tar does. An
        /// entry with no more folders than that is not extracted.
        #[arg(long, value_name = "N")]
        strip_components: Option<usize>,
        /// Write file contents only: no modification times, no Windows
        /// attributes and no Mark of the Web. Files keep only the execute
        /// bit, and folders get the default mode.
        #[arg(long)]
        content_only: bool,
        /// Refuse archives whose declared output exceeds this many bytes. By
        /// default, ZIP, tar and 7z output read in process must fit in free space.
        #[arg(long)]
        max_extracted_bytes: Option<u64>,
    },
    /// List entries (checks the index only; use test to verify payloads).
    List { archive: PathBuf },
    /// Verify archive integrity without extracting.
    Test { archive: PathBuf },
    /// Find byte-identical files in input paths (read-only; skips symlinks).
    Duplicates {
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// Print totals without listing individual paths.
        #[arg(long)]
        summary: bool,
    },
}
impl From<ZipEngine> for zipper::Engine {
    fn from(engine: ZipEngine) -> Self {
        match engine {
            ZipEngine::Auto => zipper::Engine::Auto,
            ZipEngine::Backend => zipper::Engine::Backend,
            ZipEngine::Native => zipper::Engine::Native,
            ZipEngine::NativeFast => zipper::Engine::NativeFast,
        }
    }
}

fn with_password<T>(
    given: Option<&[u8]>,
    mut run: impl FnMut(Option<&[u8]>) -> Result<T>,
) -> Result<T> {
    let needed = match run(given) {
        Err(error) if given.is_none() && error.is::<zipper::native_zip::PasswordNeeded>() => error,
        other => return other,
    };
    anyhow::ensure!(
        std::io::stdin().is_terminal(),
        "{needed}; standard input is not a terminal, so Zipper cannot ask for it"
    );
    let asked = match ask_password()? {
        Typed::Password(password) => password,
        Typed::Interrupt => {
            eprintln!("Error: the password prompt was interrupted");
            std::process::exit(INTERRUPTED);
        }
        Typed::EndOfInput => anyhow::bail!("{needed}; no password was typed"),
    };
    run(Some(&asked))
}

const LONGEST_PASSWORD: usize = 1024;
const INTERRUPTED: i32 = 130;

#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "only Unix asks for a password on a terminal")
)]
enum Typed {
    Password(Zeroizing<Vec<u8>>),
    Interrupt,
    EndOfInput,
}

#[cfg(unix)]
fn ask_password() -> Result<Typed> {
    use std::io::Write;
    let mut terminal = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")?;
    let saved = stty(&terminal, &["-g"])?;
    terminal.write_all(b"Password: ")?;
    let typed = stty(
        &terminal,
        &["-echo", "-icanon", "-isig", "min", "1", "time", "0"],
    )
    .and_then(|_| read_password(&mut terminal));
    let restored = stty(&terminal, &[saved.trim()]);
    terminal.write_all(b"\n")?;
    restored?;
    typed
}

#[cfg(unix)]
fn stty(terminal: &std::fs::File, arguments: &[&str]) -> Result<String> {
    let run = std::process::Command::new("stty")
        .args(arguments)
        .stdin(terminal.try_clone()?)
        .output()?;
    anyhow::ensure!(
        run.status.success(),
        "stty failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    Ok(String::from_utf8(run.stdout)?)
}

#[cfg(unix)]
const CONTROL_C: u8 = 0x03;
#[cfg(unix)]
const CONTROL_D: u8 = 0x04;
#[cfg(unix)]
const BACKSPACE: u8 = 0x08;
#[cfg(unix)]
const CONTROL_U: u8 = 0x15;
#[cfg(unix)]
const DELETE: u8 = 0x7f;

#[cfg(unix)]
fn read_password(terminal: &mut std::fs::File) -> Result<Typed> {
    use std::io::Read;
    let mut password = Zeroizing::new(Vec::with_capacity(LONGEST_PASSWORD));
    let mut byte = Zeroizing::new([0u8; 1]);
    loop {
        if terminal.read(&mut byte[..])? == 0 {
            return Ok(Typed::EndOfInput);
        }
        match byte[0] {
            b'\n' | b'\r' => return Ok(Typed::Password(password)),
            CONTROL_C => return Ok(Typed::Interrupt),
            CONTROL_D if password.is_empty() => return Ok(Typed::EndOfInput),
            BACKSPACE | DELETE => {
                password.pop();
            }
            CONTROL_U => password.clear(),
            control if control < b' ' => {}
            typed => {
                anyhow::ensure!(
                    password.len() < LONGEST_PASSWORD,
                    "the password is longer than {LONGEST_PASSWORD} bytes"
                );
                password.push(typed);
            }
        }
    }
}

#[cfg(not(unix))]
fn ask_password() -> Result<Typed> {
    anyhow::bail!(
        "Zipper cannot ask for a password on this system; pass --password (at most {LONGEST_PASSWORD} bytes)"
    )
}

fn main() -> Result<()> {
    let mut cli = Cli::parse();
    let password = cli
        .password
        .take()
        .map(|given| Zeroizing::new(given.into_bytes()));
    let password = password.as_ref().map(|given| given.as_slice());
    if let Some(n) = cli.threads {
        anyhow::ensure!((1..=64).contains(&n), "threads must be between 1 and 64");
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build_global()?;
    }
    anyhow::ensure!(
        cli.zip_buffer_mib.is_none() || cli.zip_engine == ZipEngine::NativeFast,
        "--zip-buffer-mib requires --zip-engine native-fast"
    );
    let buffer_bytes = cli
        .zip_buffer_mib
        .map(|mib| {
            usize::try_from(u64::from(mib) * 1024 * 1024)
                .map_err(|_| anyhow::anyhow!("ZIP buffer budget exceeds platform address space"))
        })
        .transpose()?;
    match cli.command {
        Command::Duplicates { inputs, summary } => {
            let report = zipper::duplicates::find(&inputs)?;
            for (index, group) in report.groups.iter().enumerate().filter(|_| !summary) {
                println!(
                    "Group {}: {} files, {} bytes each",
                    index + 1,
                    group.paths.len(),
                    group.size
                );
                for path in &group.paths {
                    println!("  {:?}", path);
                }
            }
            println!(
                "Scanned {} files; {} duplicate groups; {} duplicate payload bytes; {} non-regular entries skipped",
                report.scanned_files,
                report.groups.len(),
                report.duplicate_bytes,
                report.skipped_entries
            );
        }
        Command::Extract {
            archive,
            output,
            stdout,
            paths,
            include,
            exclude,
            flat,
            strip_prefix,
            strip_components,
            content_only,
            overwrite,
            existing,
            max_extracted_bytes,
        } => {
            let to_stdout = stdout.then(tempfile::tempdir).transpose()?;
            let output = match (&to_stdout, output) {
                (Some(work), _) => work.path().join("out"),
                (None, output) => output.expect("clap requires --output without --stdout"),
            };
            let spooled = if archive.as_os_str() == "-" {
                let folder = output
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty());
                let mut spool = tempfile::Builder::new()
                    .prefix(".zipper-stdin-")
                    .tempfile_in(folder.unwrap_or(std::path::Path::new(".")))?;
                std::io::copy(&mut std::io::stdin().lock(), &mut spool)?;
                Some(spool)
            } else {
                None
            };
            let archive = spooled
                .as_ref()
                .map_or(archive, |spool| spool.path().to_owned());
            with_password(password, |password| {
                zipper::extract_archive(
                    &archive,
                    &output,
                    zipper::ExtractOptions {
                        password,
                        existing: match (overwrite, existing) {
                            (true, _) => zipper::Existing::Overwrite,
                            (false, existing) => existing.unwrap_or(ExistingArg::Refuse).into(),
                        },
                        paths: &paths,
                        include: &include,
                        exclude: &exclude,
                        flat: flat || stdout,
                        strip: match (&strip_prefix, strip_components) {
                            (Some(prefix), _) => zipper::Strip::Prefix(prefix),
                            (None, Some(count)) => zipper::Strip::Components(count),
                            (None, None) => zipper::Strip::None,
                        },
                        metadata: if content_only {
                            zipper::Metadata::ContentOnly
                        } else {
                            zipper::Metadata::Restore
                        },
                        max_output_bytes: max_extracted_bytes,
                        threads: cli.threads,
                        engine: cli.zip_engine.into(),
                        zip_buffer_bytes: buffer_bytes,
                        ..Default::default()
                    },
                )
            })?;
            if to_stdout.is_some() {
                let files: Vec<PathBuf> = std::fs::read_dir(&output)?
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<std::io::Result<_>>()?;
                let [file] = &files[..] else {
                    anyhow::bail!(
                        "--stdout needs a selection of exactly one file; it matched {}",
                        files.len()
                    );
                };
                std::io::copy(
                    &mut std::fs::File::open(file)?,
                    &mut std::io::stdout().lock(),
                )?;
            }
        }
        Command::List { archive } => {
            let print = |entries: Vec<zipper::Entry>| {
                for e in entries {
                    println!(
                        "{}\t{}\t{:?}",
                        if e.directory { "dir" } else { "file" },
                        e.size,
                        e.name
                    );
                }
            };
            match zipper::choose_reader(cli.zip_engine.into(), &archive)? {
                Reader::Zip { .. } => print(zipper::native_zip::list(&archive)?),
                Reader::SevenZip => print(zipper::native_7z::list(&archive)?),
                Reader::Tar => print(zipper::native_tar::read_archive(
                    &archive,
                    None,
                    zipper::native_tar::ReadOptions::default(),
                )?),
                Reader::Stream => print(zipper::native_stream::read_archive(
                    &archive,
                    None,
                    zipper::native_stream::ReadOptions::default(),
                )?),
                Reader::Backend { format } => zipper::compat::run(
                    "l",
                    &archive,
                    None,
                    format.as_deref(),
                    zipper::Existing::Refuse,
                )?,
            }
        }
        Command::Test { archive } => {
            let report =
                |entries: Vec<zipper::Entry>| println!("Verified {} entries", entries.len());
            let reader = zipper::choose_reader(cli.zip_engine.into(), &archive)?;
            anyhow::ensure!(
                password.is_none() || matches!(reader, Reader::Zip { .. }),
                "--password applies only to ZIP archives, and Zipper does not decrypt {}",
                archive.display()
            );
            match reader {
                Reader::Zip { fast } => report(with_password(password, |password| {
                    zipper::native_zip::read_archive_with_options(
                        &archive,
                        None,
                        zipper::native_zip::ReadOptions {
                            fast,
                            threads: cli.threads,
                            buffer_bytes,
                            max_output_bytes: None,
                            password,
                            ..Default::default()
                        },
                    )
                })?),
                Reader::SevenZip => report(zipper::native_7z::read_archive(
                    &archive,
                    None,
                    zipper::native_7z::ReadOptions {
                        threads: cli.threads,
                        ..Default::default()
                    },
                )?),
                Reader::Tar => report(zipper::native_tar::read_archive(
                    &archive,
                    None,
                    zipper::native_tar::ReadOptions::default(),
                )?),
                Reader::Stream => report(zipper::native_stream::read_archive(
                    &archive,
                    None,
                    zipper::native_stream::ReadOptions::default(),
                )?),
                Reader::Backend { format } => zipper::compat::run(
                    "t",
                    &archive,
                    None,
                    format.as_deref(),
                    zipper::Existing::Refuse,
                )?,
            }
        }
    }
    Ok(())
}
