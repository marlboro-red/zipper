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
    /// Password for encrypted ZIP entries in extract and test. Other users can see it in the process list.
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
        Typed::Interrupt => return Err(PromptInterrupted.into()),
        Typed::EndOfInput => anyhow::bail!("{needed}; no password was typed"),
    };
    run(Some(&asked))
}

const LONGEST_PASSWORD: usize = 1024;
const INTERRUPTED: u8 = 130;

#[derive(Debug)]
struct PromptInterrupted;

impl std::fmt::Display for PromptInterrupted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("the password prompt was interrupted")
    }
}

impl std::error::Error for PromptInterrupted {}

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
    let mut hidden = hidden_terminal::HiddenTerminal::open()?;
    hidden.prompt(b"Password: ")?;
    read_password(&mut hidden)
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
const UTF8_CONTINUATION_MASK: u8 = 0xc0;
#[cfg(unix)]
const UTF8_CONTINUATION: u8 = 0x80;

#[cfg(unix)]
fn read_password(terminal: &mut hidden_terminal::HiddenTerminal) -> Result<Typed> {
    let mut password = Zeroizing::new(Vec::with_capacity(LONGEST_PASSWORD));
    loop {
        let Some(byte) = terminal.next_byte()? else {
            return Ok(Typed::EndOfInput);
        };
        match byte {
            b'\n' | b'\r' => return Ok(Typed::Password(password)),
            CONTROL_C => return Ok(Typed::Interrupt),
            CONTROL_D if password.is_empty() => return Ok(Typed::EndOfInput),
            CONTROL_D => {}
            BACKSPACE | DELETE => {
                while let Some(last) = password.pop() {
                    if last & UTF8_CONTINUATION_MASK != UTF8_CONTINUATION {
                        break;
                    }
                }
            }
            CONTROL_U => password.clear(),
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

#[cfg(unix)]
mod hidden_terminal {
    use anyhow::{Result, ensure};
    use std::{
        cell::UnsafeCell,
        fs::File,
        io::{ErrorKind, Read, Write},
        os::fd::AsRawFd,
        sync::atomic::{AtomicBool, AtomicI32, Ordering},
    };

    const RESTORED_ON: [libc::c_int; 4] =
        [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];
    const NO_TERMINAL: i32 = -1;

    struct SavedSettings(UnsafeCell<libc::termios>);

    // SAFETY: only the one HiddenTerminal that CLAIMED admits writes the settings, before it installs
    // the handlers that read them, and its `Drop` removes them, so no handler reads during a write.
    unsafe impl Sync for SavedSettings {}

    static CLAIMED: AtomicBool = AtomicBool::new(false);

    struct Claim;

    impl Claim {
        fn take() -> Result<Self> {
            let free = CLAIMED.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst);
            ensure!(free.is_ok(), "the password prompt is already open");
            Ok(Self)
        }
    }

    impl Drop for Claim {
        fn drop(&mut self) {
            CLAIMED.store(false, Ordering::SeqCst);
        }
    }

    // SAFETY: termios is plain data, and zeroed is a valid value for every field.
    static SAVED: SavedSettings = SavedSettings(UnsafeCell::new(unsafe { std::mem::zeroed() }));
    static TERMINAL: AtomicI32 = AtomicI32::new(NO_TERMINAL);

    extern "C" fn restore_then_die(signal: libc::c_int) {
        let terminal = TERMINAL.load(Ordering::SeqCst);
        // SAFETY: tcsetattr and raise are async-signal-safe. SAVED holds the settings `open`
        // read before it installed this handler, and SA_RESETHAND made the action default again.
        unsafe {
            libc::tcsetattr(terminal, libc::TCSAFLUSH, SAVED.0.get());
            libc::raise(signal);
        }
    }

    pub(super) struct HiddenTerminal {
        terminal: File,
        previous: [libc::sigaction; RESTORED_ON.len()],
        _claim: Claim,
    }

    impl HiddenTerminal {
        pub(super) fn open() -> Result<Self> {
            let claim = Claim::take()?;
            let terminal = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")?;
            let descriptor = terminal.as_raw_fd();
            // SAFETY: termios is plain data, and zeroed is a valid value for every field.
            let mut saved: libc::termios = unsafe { std::mem::zeroed() };
            // SAFETY: the descriptor is open for the call and `saved` is a live termios.
            let read = unsafe { libc::tcgetattr(descriptor, &mut saved) };
            ensure!(read == 0, "cannot read the terminal settings");
            // SAFETY: no handler that reads SAVED is installed yet, as SavedSettings requires.
            unsafe { *SAVED.0.get() = saved };
            TERMINAL.store(descriptor, Ordering::SeqCst);
            // SAFETY: sigaction is plain data, and zeroed is a valid value for every field.
            let mut previous: [libc::sigaction; RESTORED_ON.len()] = unsafe { std::mem::zeroed() };
            // SAFETY: as above.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = restore_then_die as extern "C" fn(libc::c_int) as usize;
            action.sa_flags = libc::SA_RESETHAND;
            for (signal, old) in RESTORED_ON.iter().zip(&mut previous) {
                // SAFETY: both pointers refer to live sigaction values for the call.
                unsafe { libc::sigaction(*signal, &action, old) };
            }
            let hidden = Self {
                terminal,
                previous,
                _claim: claim,
            };
            let mut quiet = saved;
            quiet.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG | libc::IEXTEN);
            quiet.c_cc[libc::VMIN] = 1;
            quiet.c_cc[libc::VTIME] = 0;
            // SAFETY: the descriptor is open for the call and `quiet` is a live termios.
            let set = unsafe { libc::tcsetattr(descriptor, libc::TCSAFLUSH, &quiet) };
            ensure!(set == 0, "cannot turn off echo on the terminal");
            Ok(hidden)
        }

        pub(super) fn prompt(&mut self, text: &[u8]) -> Result<()> {
            self.terminal.write_all(text)?;
            Ok(())
        }

        pub(super) fn next_byte(&mut self) -> Result<Option<u8>> {
            let mut byte = zeroize::Zeroizing::new([0u8; 1]);
            loop {
                match self.terminal.read(&mut byte[..]) {
                    Ok(0) => return Ok(None),
                    Ok(_) => return Ok(Some(byte[0])),
                    Err(error) if error.kind() == ErrorKind::Interrupted => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }

    impl Drop for HiddenTerminal {
        fn drop(&mut self) {
            // SAFETY: the descriptor is still open, and SAVED holds the settings `open` read.
            unsafe { libc::tcsetattr(self.terminal.as_raw_fd(), libc::TCSAFLUSH, SAVED.0.get()) };
            for (signal, old) in RESTORED_ON.iter().zip(&self.previous) {
                // SAFETY: `old` is the live action this signal had before `open`.
                unsafe { libc::sigaction(*signal, old, std::ptr::null_mut()) };
            }
            TERMINAL.store(NO_TERMINAL, Ordering::SeqCst);
            let _ = self.terminal.write_all(b"\n");
        }
    }
}

#[cfg(not(unix))]
fn ask_password() -> Result<Typed> {
    anyhow::bail!(
        "Zipper cannot ask for a password on this system; pass --password (at most {LONGEST_PASSWORD} bytes)"
    )
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:?}");
            if error.is::<PromptInterrupted>() {
                std::process::ExitCode::from(INTERRUPTED)
            } else {
                std::process::ExitCode::FAILURE
            }
        }
    }
}

fn run() -> Result<()> {
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
            anyhow::ensure!(
                password.is_none(),
                "duplicates reads no archives, so it takes no --password"
            );
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
            anyhow::ensure!(
                password.is_none(),
                "list reads no encrypted data, so it takes no --password"
            );
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
