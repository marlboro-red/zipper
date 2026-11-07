use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::{io::IsTerminal, path::PathBuf};
use zeroize::Zeroizing;
use zipper::Reader;

#[derive(Parser)]
#[command(
    version,
    about = "Zipper — fast, safe extraction of ZIP, tar, 7z, cpio, ar, RPM, xar, CAB, MSI and ISO archives"
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
    /// Password for encrypted ZIP entries and 7z archives in extract and test, and for a 7z header in list.
    /// Other users can see it in the process list. Without it, Zipper asks on Unix when a selected
    /// entry or a 7z header is encrypted and standard input is a terminal, and fails otherwise.
    #[arg(
        long,
        global = true,
        value_name = "PASSWORD",
        allow_hyphen_values = true
    )]
    password: Option<std::ffi::OsString>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ZipEngine {
    /// Native for ZIP, tar, cpio, ar, RPM, xar, ISO and UDF images, single compressed files, unrecognized files, and 7z, CAB and MSI it decodes; 7-Zip for other recognized formats.
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
        /// default, the output of every archive read in process, including a
        /// single compressed file, must fit in free space.
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
        Err(error)
            if given.is_none()
                && (error.is::<zipper::native_zip::PasswordNeeded>()
                    || error.is::<zipper::native_7z::PasswordNeeded>()) =>
        {
            error
        }
        other => return other,
    };
    anyhow::ensure!(
        std::io::stdin().is_terminal(),
        "{needed}; standard input is not a terminal, so Zipper cannot ask for it"
    );
    if ending_signal_received() {
        return Err(PromptInterrupted.into());
    }
    let asked = match ask_password().context("a password is needed")? {
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

fn given_password(given: std::ffi::OsString) -> Result<Zeroizing<Vec<u8>>> {
    #[cfg(unix)]
    let bytes = std::os::unix::ffi::OsStringExt::into_vec(given);
    #[cfg(not(unix))]
    let bytes = given
        .into_string()
        .map_err(|_| anyhow::anyhow!("--password must be UTF-8 on this system"))?
        .into_bytes();
    let bytes = Zeroizing::new(bytes);
    anyhow::ensure!(
        bytes.len() <= LONGEST_PASSWORD,
        "the password is longer than {LONGEST_PASSWORD} bytes"
    );
    Ok(bytes)
}

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
    let mut last = None;
    loop {
        if ending_signal_received() {
            return Ok(Typed::Interrupt);
        }
        let mut terminal = hidden_terminal::HiddenTerminal::open(last)?;
        let hidden = terminal.hide();
        last = Some(terminal.settings());
        let typed = match hidden? {
            hidden_terminal::Hidden::Yes => match terminal.prompt(b"Password: ")? {
                hidden_terminal::Hidden::Yes => read_password(&mut terminal)?,
                hidden_terminal::Hidden::Interrupted => None,
            },
            hidden_terminal::Hidden::Interrupted => None,
        };
        drop(terminal);
        let resumed = hidden_terminal::stopped_and_resumed();
        if hidden_terminal::signal_deferred() {
            return Ok(Typed::Interrupt);
        }
        match typed {
            _ if resumed => continue,
            Some(typed) => return Ok(typed),
            None => return Ok(Typed::Interrupt),
        }
    }
}

#[cfg(unix)]
const CONTROL_D: u8 = 0x04;
#[cfg(unix)]
const BACKSPACE: u8 = 0x08;
#[cfg(unix)]
const CONTROL_U: u8 = 0x15;
#[cfg(unix)]
const DELETE: u8 = 0x7f;
#[cfg(unix)]
const LONGEST_UTF8_CHARACTER: usize = 4;

#[cfg(unix)]
fn erase_last_character(password: &mut Vec<u8>) {
    let tail = password.len().saturating_sub(LONGEST_UTF8_CHARACTER);
    let whole_character = (tail..password.len()).find(|&start| {
        std::str::from_utf8(&password[start..]).is_ok_and(|text| text.chars().count() == 1)
    });
    match whole_character {
        Some(start) => password.truncate(start),
        None => {
            password.pop();
        }
    }
}

#[cfg(unix)]
fn read_password(terminal: &mut hidden_terminal::HiddenTerminal) -> Result<Option<Typed>> {
    let mut password = Zeroizing::new(Vec::with_capacity(LONGEST_PASSWORD));
    let mut too_long = false;
    loop {
        let byte = match terminal.next_byte()? {
            hidden_terminal::Read::Byte(byte) => byte,
            hidden_terminal::Read::End if too_long => break,
            hidden_terminal::Read::End => return Ok(Some(Typed::EndOfInput)),
            hidden_terminal::Read::Interrupted => return Ok(None),
        };
        match byte {
            b'\n' | b'\r' if too_long => break,
            b'\n' | b'\r' => return Ok(Some(Typed::Password(password))),
            _ if too_long => {}
            CONTROL_D if password.is_empty() => return Ok(Some(Typed::EndOfInput)),
            CONTROL_D => {}
            BACKSPACE | DELETE => erase_last_character(&mut password),
            CONTROL_U => password.clear(),
            _ if password.len() == LONGEST_PASSWORD => {
                password.clear();
                too_long = true;
            }
            typed => password.push(typed),
        }
    }
    anyhow::bail!("the password is longer than {LONGEST_PASSWORD} bytes")
}

#[cfg(unix)]
mod hidden_terminal {
    use anyhow::{Context, Result};
    use std::{
        fs::File,
        io::{ErrorKind, Read as _, Write as _},
        os::fd::AsRawFd,
        sync::atomic::{AtomicBool, Ordering},
    };

    const SIGNALS_CAUGHT_WHILE_HIDDEN: [libc::c_int; 10] = [
        libc::SIGALRM,
        libc::SIGCONT,
        libc::SIGHUP,
        libc::SIGINT,
        libc::SIGPIPE,
        libc::SIGQUIT,
        libc::SIGTERM,
        libc::SIGTSTP,
        libc::SIGTTIN,
        libc::SIGTTOU,
    ];
    const STOPS: [libc::c_int; 3] = [libc::SIGTSTP, libc::SIGTTIN, libc::SIGTTOU];
    const SIGNAL_SLOTS: usize = 32;

    static CAUGHT: [AtomicBool; SIGNAL_SLOTS] = [const { AtomicBool::new(false) }; SIGNAL_SLOTS];

    extern "C" fn record(signal: libc::c_int) {
        if let Some(caught) = usize::try_from(signal)
            .ok()
            .and_then(|slot| CAUGHT.get(slot))
        {
            caught.store(true, Ordering::SeqCst);
        }
    }

    fn caught(signal: libc::c_int) -> bool {
        usize::try_from(signal)
            .ok()
            .and_then(|slot| CAUGHT.get(slot))
            .is_some_and(|caught| caught.load(Ordering::SeqCst))
    }

    fn any_caught() -> bool {
        SIGNALS_CAUGHT_WHILE_HIDDEN
            .iter()
            .any(|&signal| caught(signal))
    }

    fn stop_caught() -> bool {
        STOPS.iter().any(|&signal| caught(signal))
    }

    static RESUMED: AtomicBool = AtomicBool::new(false);

    pub(super) fn stopped_and_resumed() -> bool {
        RESUMED.swap(false, Ordering::SeqCst)
    }

    fn in_the_foreground(terminal: &File) -> bool {
        // SAFETY: neither call takes a pointer, the descriptor is open for the call, and an error
        // from tcgetpgrp is -1, which never equals a process group, so it reads as background.
        unsafe { libc::tcgetpgrp(terminal.as_raw_fd()) == libc::getpgrp() }
    }

    static DEFERRED: [AtomicBool; SIGNAL_SLOTS] = [const { AtomicBool::new(false) }; SIGNAL_SLOTS];

    fn slot(signal: libc::c_int) -> Option<usize> {
        usize::try_from(signal)
            .ok()
            .filter(|&slot| slot < SIGNAL_SLOTS)
    }

    fn resend_caught_signals(terminal: &File) {
        RESUMED.store(stop_caught() || caught(libc::SIGCONT), Ordering::SeqCst);
        for signal in SIGNALS_CAUGHT_WHILE_HIDDEN {
            let Some(slot) = slot(signal) else { continue };
            if !CAUGHT[slot].swap(false, Ordering::SeqCst) || signal == libc::SIGCONT {
                continue;
            }
            if !STOPS.contains(&signal) {
                DEFERRED[slot].store(true, Ordering::SeqCst);
                continue;
            }
            let background_only = signal == libc::SIGTTIN || signal == libc::SIGTTOU;
            if !(background_only && in_the_foreground(terminal)) {
                // SAFETY: kill takes plain integers, and the signal's own action is back in place.
                unsafe { libc::kill(libc::getpid(), signal) };
            }
        }
    }

    pub(super) fn signal_deferred() -> bool {
        DEFERRED
            .iter()
            .any(|deferred| deferred.load(Ordering::SeqCst))
    }

    pub(super) fn take_deferred_signals() -> Vec<libc::c_int> {
        SIGNALS_CAUGHT_WHILE_HIDDEN
            .into_iter()
            .filter(|&signal| {
                slot(signal).is_some_and(|slot| DEFERRED[slot].swap(false, Ordering::SeqCst))
            })
            .collect()
    }

    pub(super) enum Hidden {
        Yes,
        Interrupted,
    }

    pub(super) enum Read {
        Byte(u8),
        End,
        Interrupted,
    }

    #[derive(Clone, Copy)]
    pub(super) struct Settings {
        saved: libc::termios,
        quiet: libc::termios,
    }

    fn same_mode(left: &libc::termios, right: &libc::termios) -> bool {
        left.c_iflag == right.c_iflag
            && left.c_lflag == right.c_lflag
            && left.c_cc[libc::VMIN] == right.c_cc[libc::VMIN]
            && left.c_cc[libc::VTIME] == right.c_cc[libc::VTIME]
    }

    const FOREGROUND_TRIES: usize = 3;

    fn wait_for_the_foreground(terminal: &File) {
        for _ in 0..FOREGROUND_TRIES {
            if in_the_foreground(terminal) {
                return;
            }
            stop_the_whole_job();
        }
    }

    fn stop_the_whole_job() {
        // SAFETY: kill takes plain integers.
        unsafe { libc::kill(0, libc::SIGTTOU) };
    }

    pub(super) struct HiddenTerminal {
        terminal: File,
        saved: libc::termios,
        quiet: libc::termios,
        hidden: bool,
        prompted: bool,
        previous: Vec<(libc::c_int, libc::sigaction)>,
    }

    impl HiddenTerminal {
        pub(super) fn open(last: Option<Settings>) -> Result<Self> {
            let terminal = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .context("cannot open the terminal")?;
            wait_for_the_foreground(&terminal);
            let current = current_settings(&terminal)?;
            let saved = match last {
                Some(last) if same_mode(&current, &last.quiet) => last.saved,
                _ => current,
            };
            let mut opened = Self {
                terminal,
                saved,
                quiet: quiet_settings(&saved),
                hidden: false,
                prompted: false,
                previous: Vec::with_capacity(SIGNALS_CAUGHT_WHILE_HIDDEN.len()),
            };
            opened.catch_signals();
            Ok(opened)
        }

        pub(super) fn settings(&self) -> Settings {
            Settings {
                saved: self.saved,
                quiet: self.quiet,
            }
        }

        fn catch_signals(&mut self) {
            let mut action = super::no_signal_action();
            action.sa_sigaction = record as extern "C" fn(libc::c_int) as usize;
            for signal in SIGNALS_CAUGHT_WHILE_HIDDEN {
                let mut old = super::no_signal_action();
                // SAFETY: a null new action only reads the current one into the live `old`.
                unsafe { libc::sigaction(signal, std::ptr::null(), &mut old) };
                if old.sa_sigaction == libc::SIG_IGN {
                    continue;
                }
                // SAFETY: both pointers refer to live sigaction values for the call, and `record`
                // only stores to lock-free atomics, which is safe in a signal handler.
                if unsafe { libc::sigaction(signal, &action, &mut old) } == 0 {
                    self.previous.push((signal, old));
                }
            }
        }

        fn set(&self, settings: &libc::termios, give_up: fn() -> bool) -> std::io::Result<bool> {
            loop {
                // SAFETY: the descriptor is open for the call and `settings` is a live termios.
                let set = unsafe {
                    libc::tcsetattr(self.terminal.as_raw_fd(), libc::TCSAFLUSH, settings)
                };
                if set == 0 {
                    return Ok(true);
                }
                let error = std::io::Error::last_os_error();
                match error.kind() {
                    ErrorKind::Interrupted if give_up() => return Ok(false),
                    ErrorKind::Interrupted => {}
                    _ => return Err(error),
                }
            }
        }

        pub(super) fn hide(&mut self) -> Result<Hidden> {
            self.hidden = true;
            if self
                .set(&self.quiet, any_caught)
                .context("cannot turn off echo on the terminal")?
            {
                Ok(Hidden::Yes)
            } else {
                Ok(Hidden::Interrupted)
            }
        }

        pub(super) fn prompt(&mut self, mut text: &[u8]) -> Result<Hidden> {
            while !text.is_empty() {
                match self.terminal.write(text) {
                    Ok(written) => text = &text[written..],
                    Err(error) if error.kind() == ErrorKind::Interrupted && any_caught() => {
                        return Ok(Hidden::Interrupted);
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => {}
                    Err(error) => return Err(error.into()),
                }
                self.prompted = true;
            }
            Ok(Hidden::Yes)
        }

        pub(super) fn next_byte(&mut self) -> Result<Read> {
            let mut byte = zeroize::Zeroizing::new([0u8; 1]);
            loop {
                if any_caught() {
                    return Ok(Read::Interrupted);
                }
                match self.terminal.read(&mut byte[..]) {
                    Ok(0) => return Ok(Read::End),
                    Ok(_) => return Ok(Read::Byte(byte[0])),
                    Err(error) if error.kind() == ErrorKind::Interrupted => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }

    impl Drop for HiddenTerminal {
        fn drop(&mut self) {
            if self.hidden {
                let _ = self.set(&self.saved, || caught(libc::SIGTTOU));
            }
            if self.prompted {
                let _ = self.terminal.write(b"\n");
            }
            for (signal, old) in &self.previous {
                // SAFETY: `old` is the live action this signal had before `open`.
                unsafe { libc::sigaction(*signal, old, std::ptr::null_mut()) };
            }
            resend_caught_signals(&self.terminal);
        }
    }

    fn quiet_settings(saved: &libc::termios) -> libc::termios {
        let mut quiet = *saved;
        quiet.c_lflag &= !(libc::ECHO | libc::ICANON | libc::IEXTEN);
        quiet.c_iflag &= !libc::IXON;
        quiet.c_cc[libc::VMIN] = 1;
        quiet.c_cc[libc::VTIME] = 0;
        quiet
    }

    fn current_settings(terminal: &File) -> Result<libc::termios> {
        // SAFETY: termios is plain data, and zeroed is a valid value for every field.
        let mut settings: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: the descriptor is open for the call and `settings` is a live termios.
        let read = unsafe { libc::tcgetattr(terminal.as_raw_fd(), &mut settings) };
        if read != 0 {
            return Err(std::io::Error::last_os_error())
                .context("cannot read the terminal settings");
        }
        Ok(settings)
    }
}

#[cfg(not(unix))]
fn ask_password() -> Result<Typed> {
    anyhow::bail!(
        "Zipper cannot ask for a password on this system; pass --password (at most {LONGEST_PASSWORD} bytes)"
    )
}

#[cfg(unix)]
mod stop_on_signal {
    use std::sync::atomic::{AtomicBool, Ordering};

    const ENDING: [libc::c_int; 4] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];
    const SIGNAL_SLOTS: usize = 32;

    pub(super) static CANCEL: AtomicBool = AtomicBool::new(false);
    static RECEIVED: [AtomicBool; SIGNAL_SLOTS] = [const { AtomicBool::new(false) }; SIGNAL_SLOTS];

    extern "C" fn record(signal: libc::c_int) {
        if let Some(received) = usize::try_from(signal)
            .ok()
            .and_then(|slot| RECEIVED.get(slot))
        {
            received.store(true, Ordering::SeqCst);
            CANCEL.store(true, Ordering::SeqCst);
        }
    }

    pub(super) struct Catching {
        previous: Vec<(libc::c_int, libc::sigaction)>,
    }

    impl Catching {
        pub(super) fn start() -> Self {
            let mut action = super::no_signal_action();
            action.sa_sigaction = record as extern "C" fn(libc::c_int) as usize;
            action.sa_flags = libc::SA_RESTART;
            let mut previous = Vec::with_capacity(ENDING.len());
            for signal in ENDING {
                let mut old = super::no_signal_action();
                // SAFETY: a null new action only reads the current one into the live `old`.
                unsafe { libc::sigaction(signal, std::ptr::null(), &mut old) };
                if old.sa_sigaction == libc::SIG_IGN {
                    continue;
                }
                // SAFETY: both pointers refer to live sigaction values for the call, and `record`
                // only stores to lock-free atomics, which is safe in a signal handler.
                if unsafe { libc::sigaction(signal, &action, &mut old) } == 0 {
                    previous.push((signal, old));
                }
            }
            Self { previous }
        }

        pub(super) fn keep_until_exit(self) {
            std::mem::forget(self);
        }
    }

    impl Drop for Catching {
        fn drop(&mut self) {
            for (signal, old) in &self.previous {
                // SAFETY: `old` is the live action this signal had before `start`.
                unsafe { libc::sigaction(*signal, old, std::ptr::null_mut()) };
            }
        }
    }

    pub(super) fn take_received_signals() -> Vec<libc::c_int> {
        ENDING
            .into_iter()
            .filter(|&signal| {
                usize::try_from(signal)
                    .ok()
                    .and_then(|slot| RECEIVED.get(slot))
                    .is_some_and(|received| received.swap(false, Ordering::SeqCst))
            })
            .collect()
    }

    #[cfg(test)]
    pub(super) fn receive(signal: libc::c_int) {
        record(signal);
    }
}

#[cfg(unix)]
fn no_signal_action() -> libc::sigaction {
    // SAFETY: sigaction is plain data, and zeroed is a valid value for every field: no
    // handler, no flags and an empty mask.
    unsafe { std::mem::zeroed() }
}

const COPY_CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(10);

fn copy_until_cancelled(
    mut from: impl std::io::Read + Send + 'static,
    mut to: impl std::io::Write + Send + 'static,
) -> Result<()> {
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let copied = std::io::copy(&mut from, &mut to).and_then(|_| to.flush());
        let _ = done.send(copied);
    });
    loop {
        match finished.recv_timeout(COPY_CANCEL_POLL) {
            Ok(copied) => return Ok(copied?),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                anyhow::ensure!(!ending_signal_received(), zipper::Cancelled);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("the copy stopped")
            }
        }
    }
}

fn ending_signal_received() -> bool {
    cancel_on_signal().is_some_and(|cancel| cancel.load(std::sync::atomic::Ordering::SeqCst))
}

#[cfg(unix)]
fn cancel_on_signal() -> Option<&'static std::sync::atomic::AtomicBool> {
    Some(&stop_on_signal::CANCEL)
}

#[cfg(not(unix))]
fn cancel_on_signal() -> Option<&'static std::sync::atomic::AtomicBool> {
    None
}

#[cfg(unix)]
fn catch_ending_signals() -> stop_on_signal::Catching {
    stop_on_signal::Catching::start()
}

#[cfg(not(unix))]
struct NoSignalsCaught;

#[cfg(not(unix))]
impl NoSignalsCaught {
    fn keep_until_exit(self) {}
}

#[cfg(not(unix))]
fn catch_ending_signals() -> NoSignalsCaught {
    NoSignalsCaught
}

#[cfg(unix)]
fn signals_that_ended_the_run(succeeded: bool) -> Vec<libc::c_int> {
    let mut ending = hidden_terminal::take_deferred_signals();
    ending.extend(stop_on_signal::take_received_signals());
    if succeeded {
        ending.clear();
    }
    ending
}

fn raise_signals_that_ended_the_run(succeeded: bool) {
    #[cfg(unix)]
    for signal in signals_that_ended_the_run(succeeded) {
        // SAFETY: kill takes plain integers, and every handler of ours is gone again.
        unsafe { libc::kill(libc::getpid(), signal) };
    }
    #[cfg(not(unix))]
    let _ = succeeded;
}

fn main() -> std::process::ExitCode {
    let ran = run();
    raise_signals_that_ended_the_run(ran.is_ok());
    match ran {
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
    let password = cli.password.take().map(given_password).transpose()?;
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
            let catching = catch_ending_signals();
            let to_stdout = stdout.then(tempfile::tempdir).transpose()?;
            let output = match (&to_stdout, output) {
                (Some(work), _) => work.path().join("out"),
                (None, output) => output.expect("clap requires --output without --stdout"),
            };
            let spooled = if archive.as_os_str() == "-" {
                let folder = output
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty());
                let spool = tempfile::Builder::new()
                    .prefix(".zipper-stdin-")
                    .tempfile_in(folder.unwrap_or(std::path::Path::new(".")))?;
                copy_until_cancelled(std::io::stdin(), spool.as_file().try_clone()?)?;
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
                        control: zipper::Control {
                            progress: None,
                            cancel: cancel_on_signal(),
                        },
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
                copy_until_cancelled(std::fs::File::open(file)?, std::io::stdout())?;
            }
            catching.keep_until_exit();
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
            zipper::read_volumes(&archive, || {
                match zipper::choose_reader(cli.zip_engine.into(), &archive)? {
                    Reader::SevenZip => print(with_password(password, |password| {
                        zipper::native_7z::list_with_password(&archive, password)
                    })?),
                    _ if password.is_some() => anyhow::bail!(
                        "list reads no encrypted data outside a 7z header, so it takes no --password"
                    ),
                    Reader::Zip { .. } => print(zipper::native_zip::list(&archive)?),
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
                    Reader::Cpio => print(zipper::native_cpio::read_archive(
                        &archive,
                        None,
                        zipper::native_cpio::ReadOptions::default(),
                    )?),
                    Reader::Ar => print(zipper::native_ar::read_archive(
                        &archive,
                        None,
                        zipper::native_ar::ReadOptions::default(),
                    )?),
                    Reader::Rpm => print(zipper::native_rpm::read_archive(
                        &archive,
                        None,
                        zipper::native_rpm::ReadOptions::default(),
                    )?),
                    Reader::Xar => print(zipper::native_xar::read_archive(
                        &archive,
                        None,
                        zipper::native_xar::ReadOptions::default(),
                    )?),
                    Reader::Cab => print(zipper::native_cab::read_archive(
                        &archive,
                        None,
                        zipper::native_cab::ReadOptions::default(),
                    )?),
                    Reader::Lzh => print(zipper::native_lzh::read_archive(
                        &archive,
                        None,
                        zipper::native_lzh::ReadOptions::default(),
                    )?),
                    Reader::Arj => print(zipper::native_arj::read_archive(
                        &archive,
                        None,
                        zipper::native_arj::ReadOptions::default(),
                    )?),
                    Reader::Iso => print(zipper::native_iso::read_archive(
                        &archive,
                        None,
                        zipper::native_iso::ReadOptions::default(),
                    )?),
                    Reader::Msi => {
                        let _catching = catch_ending_signals();
                        print(zipper::native_msi::read_archive(
                            &archive,
                            None,
                            zipper::native_msi::ReadOptions {
                                control: zipper::Control {
                                    cancel: cancel_on_signal(),
                                    ..zipper::Control::default()
                                },
                                ..zipper::native_msi::ReadOptions::default()
                            },
                        )?)
                    }
                    Reader::Backend { format } => {
                        let _catching = catch_ending_signals();
                        zipper::compat::run_until_cancelled(
                            "l",
                            &archive,
                            None,
                            format.as_deref(),
                            zipper::Existing::Refuse,
                            cancel_on_signal(),
                        )?
                    }
                }
                Ok::<_, anyhow::Error>(())
            })?;
        }
        Command::Test { archive } => {
            let report =
                |entries: Vec<zipper::Entry>| println!("Verified {} entries", entries.len());
            zipper::read_volumes(&archive, || {
                let reader = zipper::choose_reader(cli.zip_engine.into(), &archive)?;
                anyhow::ensure!(
                    password.is_none() || matches!(reader, Reader::Zip { .. } | Reader::SevenZip),
                    "--password applies only to ZIP and 7z archives, and Zipper does not decrypt {}",
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
                    Reader::SevenZip => report(with_password(password, |password| {
                        zipper::native_7z::read_archive(
                            &archive,
                            None,
                            zipper::native_7z::ReadOptions {
                                threads: cli.threads,
                                password,
                                ..Default::default()
                            },
                        )
                    })?),
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
                    Reader::Cpio => report(zipper::native_cpio::read_archive(
                        &archive,
                        None,
                        zipper::native_cpio::ReadOptions::default(),
                    )?),
                    Reader::Ar => report(zipper::native_ar::read_archive(
                        &archive,
                        None,
                        zipper::native_ar::ReadOptions::default(),
                    )?),
                    Reader::Rpm => report(zipper::native_rpm::read_archive(
                        &archive,
                        None,
                        zipper::native_rpm::ReadOptions::default(),
                    )?),
                    Reader::Xar => report(zipper::native_xar::read_archive(
                        &archive,
                        None,
                        zipper::native_xar::ReadOptions::default(),
                    )?),
                    Reader::Cab => report(zipper::native_cab::read_archive(
                        &archive,
                        None,
                        zipper::native_cab::ReadOptions::default(),
                    )?),
                    Reader::Lzh => report(zipper::native_lzh::read_archive(
                        &archive,
                        None,
                        zipper::native_lzh::ReadOptions::default(),
                    )?),
                    Reader::Arj => report(zipper::native_arj::read_archive(
                        &archive,
                        None,
                        zipper::native_arj::ReadOptions::default(),
                    )?),
                    Reader::Iso => report(zipper::native_iso::read_archive(
                        &archive,
                        None,
                        zipper::native_iso::ReadOptions::default(),
                    )?),
                    Reader::Msi => {
                        let _catching = catch_ending_signals();
                        report(zipper::native_msi::read_archive(
                            &archive,
                            None,
                            zipper::native_msi::ReadOptions {
                                control: zipper::Control {
                                    cancel: cancel_on_signal(),
                                    ..zipper::Control::default()
                                },
                                ..zipper::native_msi::ReadOptions::default()
                            },
                        )?)
                    }
                    Reader::Backend { format } => {
                        let _catching = catch_ending_signals();
                        zipper::compat::run_until_cancelled(
                            "t",
                            &archive,
                            None,
                            format.as_deref(),
                            zipper::Existing::Refuse,
                            cancel_on_signal(),
                        )?
                    }
                }
                Ok::<_, anyhow::Error>(())
            })?;
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_signal_after_a_finished_run_is_not_raised_and_one_before_is() {
        stop_on_signal::receive(libc::SIGTERM);
        assert_eq!(signals_that_ended_the_run(true), Vec::<libc::c_int>::new());
        assert_eq!(signals_that_ended_the_run(false), Vec::<libc::c_int>::new());
        stop_on_signal::receive(libc::SIGTERM);
        stop_on_signal::receive(libc::SIGHUP);
        assert_eq!(
            signals_that_ended_the_run(false),
            [libc::SIGTERM, libc::SIGHUP]
        );
    }
}
