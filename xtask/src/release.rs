use crate::verify::{self, Verification};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, mpsc},
    thread,
};

pub(crate) const RECORD: &str = "verification/release-checks.txt";
const RECORD_HEADER: &str = "\
# Written by `cargo xtask release-checks` and `cargo xtask verify-sync`; do not edit by hand.
# Each `checked` line names a section, the key of every tracked file but docs and these records
# plus the section's own lines in verification/coverage.txt, and the tools its full checks passed on.
";
const PRINTED: &str = "release record: ";
const TOOLS_AFTER: &str = " | ";
pub(crate) const DEFAULT_JOBS: usize = 3;

pub(crate) struct Options {
    pub(crate) all: bool,
    pub(crate) jobs: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) checked: BTreeMap<String, String>,
}

impl Record {
    fn parse(text: &str) -> Result<Self> {
        let mut checked = BTreeMap::new();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let entry = line.strip_prefix("checked = ").with_context(|| {
                format!(
                    "{RECORD}:{}: expected `checked = NAME KEY | TOOLS`",
                    index + 1
                )
            })?;
            let (name, passed_on) = entry.split_once(' ').with_context(|| {
                format!(
                    "{RECORD}:{}: expected `checked = NAME KEY | TOOLS`",
                    index + 1
                )
            })?;
            ensure!(
                passed_on.contains(TOOLS_AFTER),
                "{RECORD}:{}: expected `checked = NAME KEY | TOOLS`",
                index + 1
            );
            checked.insert(name.to_owned(), passed_on.to_owned());
        }
        Ok(Self { checked })
    }

    fn render(&self) -> String {
        let mut text = RECORD_HEADER.to_owned();
        for (name, passed_on) in &self.checked {
            text.push_str(&format!("checked = {name} {passed_on}\n"));
        }
        text
    }
}

pub(crate) fn read_record(root: &Path) -> Result<Record> {
    match fs::read_to_string(root.join(RECORD)) {
        Ok(text) => Record::parse(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Record::default()),
        Err(error) => Err(error).with_context(|| format!("cannot read {RECORD}")),
    }
}

fn write_record(root: &Path, record: &Record) -> Result<()> {
    verify::write_atomically(&root.join(RECORD), &record.render())
}

pub(crate) fn record_checked(root: &Path, name: &str, passed_on: &str) -> Result<()> {
    let mut record = read_record(root)?;
    record.checked.insert(name.to_owned(), passed_on.to_owned());
    write_record(root, &record)
}

fn outside_the_key(path: &str) -> bool {
    path.ends_with(".md") || path == verify::MANIFEST || path == RECORD
}

fn tracked_code_hash(root: &Path) -> Result<u64> {
    let listed = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .context("cannot run `git ls-files`")?;
    ensure!(
        listed.status.success(),
        "`git ls-files` exited with {}: {}",
        listed.status,
        String::from_utf8_lossy(&listed.stderr).trim()
    );
    let mut paths: Vec<&str> = std::str::from_utf8(&listed.stdout)?
        .split('\0')
        .filter(|path| !path.is_empty() && !outside_the_key(path))
        .collect();
    paths.sort_unstable();
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for path in paths {
        let bytes = match fs::read(root.join(path)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => b"\0deleted".to_vec(),
            Err(error) => return Err(error).with_context(|| format!("cannot read {path}")),
        };
        hash = verify::fnv1a(hash, path.bytes().chain([0]).chain(bytes).chain([0]));
    }
    Ok(hash)
}

fn own_lines(manifest: &str, name: &str) -> String {
    let header = format!("[{name}]");
    let mut lines = manifest.lines().skip_while(|line| *line != header);
    let mut own = String::new();
    if let Some(first) = lines.next() {
        own.push_str(first);
        own.push('\n');
    }
    for line in lines.take_while(|line| !line.starts_with('[')) {
        if !line.starts_with("fingerprint = ") {
            own.push_str(line);
            own.push('\n');
        }
    }
    own
}

pub(crate) fn checked_key(
    root: &Path,
    verification: &Verification,
    fingerprint: &str,
) -> Result<String> {
    let manifest = verify::read_source(root, verify::MANIFEST)?;
    Ok(key_of(
        tracked_code_hash(root)?,
        &manifest,
        verification,
        fingerprint,
    ))
}

fn key_of(tree: u64, manifest: &str, verification: &Verification, fingerprint: &str) -> String {
    let hash = verify::fnv1a(
        tree,
        fingerprint
            .bytes()
            .chain([0])
            .chain(own_lines(manifest, &verification.name).bytes()),
    );
    format!("{hash:016x}")
}

fn is_model_check(check: &str) -> bool {
    check.contains("scripts/check_model.py")
}

fn version_of(root: &Path, program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("cannot run `{program} {}`", args.join(" ")))?;
    ensure!(
        output.status.success(),
        "`{program} {}` exited with {}: {}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)?.trim().replace('\n', " "))
}

fn model_runner_version() -> Result<String> {
    let runner = std::env::var_os("STATECHART")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| {
                PathBuf::from(home).join(".claude/skills/statechart/scripts/statechart")
            })
        })
        .context("neither STATECHART nor HOME is set, so the TLC runner cannot be found")?;
    let folder = runner
        .parent()
        .with_context(|| format!("{} has no folder", runner.display()))?;
    ensure!(
        runner.is_file(),
        "no TLC runner at {}; install the pstack statechart skill or set STATECHART",
        runner.display()
    );
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for entry in walkdir::WalkDir::new(folder).sort_by_file_name() {
        let entry = entry?;
        if entry.file_type().is_file() {
            let relative = entry.path().strip_prefix(folder)?.to_string_lossy();
            hash = verify::fnv1a(
                hash,
                relative.bytes().chain([0]).chain(fs::read(entry.path())?),
            );
        }
    }
    Ok(format!("statechart {hash:016x}"))
}

#[derive(Default)]
pub(crate) struct Tools {
    rustc: Option<String>,
    kani: Option<String>,
    model_runner: Option<String>,
}

impl Tools {
    pub(crate) fn of(&mut self, root: &Path, verification: &Verification) -> Result<String> {
        let mut versions = Vec::new();
        if self.rustc.is_none() {
            self.rustc = Some(version_of(root, "rustc", &["-V"])?);
        }
        versions.extend(self.rustc.clone());
        let checks = &verification.full_checks;
        if checks.iter().any(|check| is_kani_check(check)) {
            if self.kani.is_none() {
                self.kani = Some(version_of(root, "cargo", &["kani", "--version"])?);
            }
            versions.extend(self.kani.clone());
        }
        if checks.iter().any(|check| is_model_check(check)) {
            if self.model_runner.is_none() {
                self.model_runner = Some(model_runner_version()?);
            }
            versions.extend(self.model_runner.clone());
        }
        Ok(versions.join("; "))
    }
}

pub(crate) fn passed_on(key: &str, tools: &str) -> String {
    format!("{key}{TOOLS_AFTER}{tools}")
}

pub(crate) fn printed_line(name: &str, fingerprint: &str, passed_on: &str) -> String {
    format!("{PRINTED}checked {name} {fingerprint} {passed_on}")
}

pub(crate) struct Job {
    pub(crate) section: String,
    pub(crate) checks: Vec<String>,
}

#[derive(Default)]
pub(crate) struct Plan {
    pub(crate) run: Vec<Job>,
    pub(crate) skipped: Vec<String>,
    pub(crate) without_full_check: Vec<String>,
}

pub(crate) fn plan(
    sections: &[Verification],
    mut state: impl FnMut(&Verification) -> Result<(String, String)>,
    record: &Record,
    all: bool,
) -> Result<Plan> {
    let mut plan = Plan::default();
    let mut stale = Vec::new();
    let mut slow = Vec::new();
    let mut quick = Vec::new();
    for verification in sections {
        let (current, passed_on) = state(verification)?;
        if current != verification.fingerprint {
            stale.push(verify::sync_instruction(verification));
            continue;
        }
        if verification.full_checks.is_empty() {
            plan.without_full_check.push(verification.name.clone());
            continue;
        }
        if !all && record.checked.get(&verification.name) == Some(&passed_on) {
            plan.skipped.push(verification.name.clone());
            continue;
        }
        let job = Job {
            section: verification.name.clone(),
            checks: verification.full_checks.clone(),
        };
        if verification.slow {
            slow.push(job);
        } else {
            quick.push(job);
        }
    }
    ensure!(
        stale.is_empty(),
        "release-checks records no fingerprint, so sync each changed section by its skill first:\n  {}",
        stale.join("\n  ")
    );
    plan.run = slow;
    plan.run.extend(quick);
    Ok(plan)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stream {
    Out,
    Err,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Failed {
    pub(crate) section: String,
    pub(crate) check: String,
    pub(crate) why: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Line(Stream, String),
    Passed(String),
    Failed(Failed),
}

pub(crate) fn run_jobs<F>(
    jobs: Vec<Job>,
    workers: usize,
    command_for: F,
    mut on_event: impl FnMut(Event),
) where
    F: Fn(&str, usize) -> Result<Command> + Sync,
{
    let queue = Mutex::new(VecDeque::from(jobs));
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| {
        for lane in 0..workers.max(1) {
            let sender = sender.clone();
            let (queue, command_for) = (&queue, &command_for);
            scope.spawn(move || {
                loop {
                    let next = queue
                        .lock()
                        .expect("no worker panics holding the queue")
                        .pop_front();
                    let Some(job) = next else { break };
                    let event = match run_job(&job, lane, command_for, &sender) {
                        None => Event::Passed(job.section),
                        Some(failed) => Event::Failed(failed),
                    };
                    if sender.send(event).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        for event in receiver {
            on_event(event);
        }
    });
}

fn run_job<F>(
    job: &Job,
    lane: usize,
    command_for: &F,
    sender: &mpsc::Sender<Event>,
) -> Option<Failed>
where
    F: Fn(&str, usize) -> Result<Command>,
{
    for check in &job.checks {
        let failed = |why: String| {
            Some(Failed {
                section: job.section.clone(),
                check: check.clone(),
                why,
            })
        };
        let _ = sender.send(Event::Line(
            Stream::Out,
            format!("[{}] running {check}", job.section),
        ));
        let mut command = match command_for(check, lane) {
            Ok(command) => command,
            Err(error) => return failed(format!("{error:#}")),
        };
        let mut child = match command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => return failed(format!("could not start: {error}")),
        };
        let pipes: [(Option<Box<dyn Read + Send>>, Stream); 2] = [
            (
                child.stdout.take().map(|pipe| Box::new(pipe) as _),
                Stream::Out,
            ),
            (
                child.stderr.take().map(|pipe| Box::new(pipe) as _),
                Stream::Err,
            ),
        ];
        let counts_tests = verify::runs_tests(check);
        let (status, printed) = thread::scope(|scope| {
            let forwarders: Vec<_> =
                pipes
                    .into_iter()
                    .filter_map(|(pipe, stream)| {
                        let pipe = pipe?;
                        let sender = sender.clone();
                        let section = job.section.as_str();
                        Some(scope.spawn(move || {
                            forward_lines(pipe, stream, section, &sender, counts_tests)
                        }))
                    })
                    .collect();
            let status = child.wait();
            let printed: String = forwarders
                .into_iter()
                .filter_map(|forwarder| forwarder.join().ok())
                .collect();
            (status, printed)
        });
        match status {
            Ok(status) if !status.success() => return failed(format!("exited with {status}")),
            Ok(_) if counts_tests && verify::ran_no_tests(&printed) => {
                return failed(verify::RAN_NO_TEST.to_owned());
            }
            Ok(_) => {}
            Err(error) => return failed(format!("could not be waited for: {error}")),
        }
    }
    None
}

fn forward_lines(
    pipe: impl Read,
    stream: Stream,
    section: &str,
    sender: &mpsc::Sender<Event>,
    keep: bool,
) -> String {
    let mut kept = String::new();
    for line in BufReader::new(pipe).split(b'\n') {
        let Ok(line) = line else { break };
        let text = String::from_utf8_lossy(&line);
        let text = text.strip_suffix('\r').unwrap_or(&text);
        if keep {
            kept.push_str(text);
            kept.push('\n');
        }
        if sender
            .send(Event::Line(stream, format!("[{section}] {text}")))
            .is_err()
        {
            break;
        }
    }
    kept
}

pub(crate) fn is_reach_check(check: &str) -> bool {
    let words: Vec<&str> = check
        .split_whitespace()
        .filter(|word| !word.contains('='))
        .take(3)
        .collect();
    words.starts_with(&["cargo", "xtask", "kani-reach"])
}

fn split_reach_checks(jobs: Vec<Job>) -> (Vec<Job>, Vec<Job>) {
    let mut reach = Vec::new();
    let mut rest = Vec::new();
    for job in jobs {
        let (reach_checks, other_checks): (Vec<String>, Vec<String>) = job
            .checks
            .into_iter()
            .partition(|check| is_reach_check(check));
        if !reach_checks.is_empty() {
            reach.push(Job {
                section: job.section.clone(),
                checks: reach_checks,
            });
        }
        rest.push(Job {
            section: job.section,
            checks: other_checks,
        });
    }
    (reach, rest)
}

pub(crate) fn run_reach_first<F>(
    jobs: Vec<Job>,
    workers: usize,
    command_for: F,
    mut on_event: impl FnMut(Event),
) -> bool
where
    F: Fn(&str, usize) -> Result<Command> + Sync,
{
    let (reach, rest) = split_reach_checks(jobs);
    let mut reached = true;
    run_jobs(reach, workers, &command_for, |event| match event {
        Event::Passed(_) => {}
        Event::Failed(failed) => {
            reached = false;
            on_event(Event::Failed(failed));
        }
        line => on_event(line),
    });
    if reached {
        run_jobs(rest, workers, command_for, on_event);
    }
    reached
}

fn is_kani_check(check: &str) -> bool {
    let words: Vec<&str> = check
        .split_whitespace()
        .filter(|word| !word.contains('='))
        .take(3)
        .collect();
    words.starts_with(&["cargo", "kani"]) || words.starts_with(&["cargo", "xtask", "kani-reach"])
}

pub(crate) fn lane_target(check: &str, lane: usize, target: &Path) -> Option<PathBuf> {
    (lane > 0 && is_kani_check(check))
        .then(|| target.join("release-lanes").join(format!("kani-{lane}")))
}

fn record_pass(
    root: &Path,
    sections: &[Verification],
    name: &str,
    planned: &BTreeMap<String, (String, String)>,
    record: &mut Record,
) -> Result<String> {
    let verification = sections
        .iter()
        .find(|verification| verification.name == name)
        .with_context(|| format!("no section named {name}"))?;
    let (fingerprint, passed_on) = planned
        .get(name)
        .with_context(|| format!("{name} was never planned"))?;
    let current = verify::fingerprint(root, verification)?;
    let key = checked_key(root, verification, &current)?;
    ensure!(
        current == *fingerprint && passed_on.starts_with(&format!("{key}{TOOLS_AFTER}")),
        "the code or its section changed while the check ran; nothing was recorded"
    );
    record.checked.insert(name.to_owned(), passed_on.clone());
    write_record(root, record)?;
    Ok(printed_line(name, fingerprint, passed_on))
}

pub(crate) fn run_full_checks(root: &Path, target: &Path, options: &Options) -> Result<()> {
    verify::fail_on_owed_slow_checks(root)?;
    let mut record = read_record(root)?;
    let sections = verify::manifest(root)?;
    let mut planned = BTreeMap::new();
    let mut tools = Tools::default();
    let tree = tracked_code_hash(root)?;
    let manifest = verify::read_source(root, verify::MANIFEST)?;
    let plan = plan(
        &sections,
        |verification| {
            let current = verify::fingerprint(root, verification)?;
            if current != verification.fingerprint || verification.full_checks.is_empty() {
                return Ok((current, String::new()));
            }
            let key = key_of(tree, &manifest, verification, &current);
            let passed_on = passed_on(&key, &tools.of(root, verification)?);
            planned.insert(
                verification.name.clone(),
                (current.clone(), passed_on.clone()),
            );
            Ok((current, passed_on))
        },
        &record,
        options.all,
    )?;
    for name in &plan.without_full_check {
        println!(
            "xtask: release check {name}: no full check; the gate's verify step keeps it in step with its skill"
        );
    }
    for name in &plan.skipped {
        println!(
            "xtask: release check {name}: skipped, since its full checks passed on this code with these tools"
        );
    }
    println!(
        "xtask: release checks: running {} section(s) on {} worker(s), each `kani-reach` check first, skipping {}",
        plan.run.len(),
        options.jobs.max(1),
        plan.skipped.len()
    );
    let mut failed = Vec::new();
    let reached = run_reach_first(
        plan.run,
        options.jobs,
        |check, lane| {
            let mut command = verify::full_check_command(root, check)?;
            if let Some(folder) = lane_target(check, lane, target) {
                command.env("CARGO_TARGET_DIR", folder);
            }
            Ok(command)
        },
        |event| match event {
            Event::Line(Stream::Out, text) => println!("{text}"),
            Event::Line(Stream::Err, text) => eprintln!("{text}"),
            Event::Passed(name) => match record_pass(root, &sections, &name, &planned, &mut record)
            {
                Ok(line) => println!("{line}"),
                Err(error) => failed.push(format!("{name}: {error:#}")),
            },
            Event::Failed(failure) => {
                let line = format!("{}: `{}` {}", failure.section, failure.check, failure.why);
                eprintln!("xtask: release check failed: {line}");
                failed.push(line);
            }
        },
    );
    ensure!(
        reached,
        "a `cargo xtask kani-reach` check failed, so the longer full checks did not run:\n  {}",
        failed.join("\n  ")
    );
    ensure!(
        failed.is_empty(),
        "release checks failed in {} section(s):\n  {}",
        failed.len(),
        failed.join("\n  ")
    );
    Ok(())
}

fn parse_printed(printed: &str) -> Result<Vec<(String, String, String)>> {
    let mut checked = Vec::new();
    for line in printed.lines() {
        let Some(rest) = line
            .strip_prefix(PRINTED)
            .and_then(|rest| rest.strip_prefix("checked "))
        else {
            continue;
        };
        let malformed = || format!("the second machine printed a malformed record line: {line}");
        let (name, rest) = rest.split_once(' ').with_context(malformed)?;
        let (fingerprint, passed_on) = rest.split_once(' ').with_context(malformed)?;
        ensure!(passed_on.contains(TOOLS_AFTER), malformed());
        checked.push((
            name.to_owned(),
            fingerprint.to_owned(),
            passed_on.to_owned(),
        ));
    }
    Ok(checked)
}

pub(crate) fn apply_printed_record(root: &Path, printed: &str) -> Result<String> {
    let checked = parse_printed(printed)?;
    let sections = verify::manifest(root)?;
    let mut record = read_record(root)?;
    for (name, fingerprint, passed_on) in &checked {
        let verification = sections
            .iter()
            .find(|verification| &verification.name == name)
            .with_context(|| {
                format!(
                    "the second machine checked {name}, which {} lacks",
                    verify::MANIFEST
                )
            })?;
        let local = verify::fingerprint(root, verification)?;
        let key = checked_key(root, verification, &local)?;
        ensure!(
            local == *fingerprint
                && verification.fingerprint == *fingerprint
                && passed_on.starts_with(&format!("{key}{TOOLS_AFTER}")),
            "{name}: the second machine checked fingerprint {fingerprint} and {passed_on}, but this tree has fingerprint {local}, {} recorded, and key {key}; nothing more was recorded",
            verification.fingerprint
        );
        record.checked.insert(name.clone(), passed_on.clone());
        write_record(root, &record)?;
    }
    Ok(format!(
        "recorded {} section(s) checked on the second machine; commit {RECORD}",
        checked.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(name: &str, fingerprint: &str, checks: &[&str], slow: bool) -> Verification {
        Verification {
            name: name.to_owned(),
            skill: format!("{name}.md"),
            fingerprint: fingerprint.to_owned(),
            slow,
            full_checks: checks.iter().map(|check| (*check).to_owned()).collect(),
            items: Vec::new(),
        }
    }

    #[test]
    fn a_stale_or_owed_section_stops_the_release_run_before_any_check() {
        for slow in [false, true] {
            let sections = [
                section("fresh", "f1", &["true"], false),
                section("model-only", "before-the-code-changed", &["true"], slow),
            ];
            let state = |verification: &Verification| -> Result<(String, String)> {
                let current = match verification.name.as_str() {
                    "fresh" => "f1",
                    _ => "after-the-code-changed",
                };
                Ok((current.to_owned(), format!("key-{current}")))
            };
            for all in [false, true] {
                let refusal = match plan(&sections, state, &Record::default(), all) {
                    Ok(plan) => panic!(
                        "a stale section ran: {:?}",
                        plan.run.iter().map(|job| &job.section).collect::<Vec<_>>()
                    ),
                    Err(refusal) => format!("{refusal:#}"),
                };
                assert!(
                    refusal.contains("cargo xtask verify-sync model-only"),
                    "{refusal}"
                );
            }
        }
    }

    #[cfg(unix)]
    fn git_in(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(root)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[cfg(unix)]
    #[test]
    fn the_checked_key_follows_every_tracked_code_file_and_not_docs_or_other_sections() {
        let work = tempfile::tempdir().unwrap();
        let root = work.path();
        let coverage = "[one]\nskill = one.md\nfingerprint = 01\nfull-check = cargo test --release --test one\n\n\
                        [two]\nskill = two.md\nfingerprint = 02\nfull-check = cargo test --release --test two\n";
        for (path, text) in [
            ("src/lib.rs", "fn f() {}"),
            ("tests/one.rs", "#[test] fn t() {}"),
            ("tests/common/mod.rs", "pub fn helper() {}"),
            ("tests/data/fixture.bin", "bytes"),
            ("Cargo.lock", "version = 4"),
            ("README.md", "# Zipper"),
            (verify::MANIFEST, coverage),
            (RECORD, "checked = two 1 | rustc 1\n"),
        ] {
            fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            fs::write(root.join(path), text).unwrap();
        }
        git_in(root, &["init", "-q"]);
        git_in(root, &["add", "-A"]);
        let one = section("one", "01", &["cargo test --release --test one"], false);
        let key = || checked_key(root, &one, "01").unwrap();
        let original = key();
        assert_eq!(original, key());
        let edit = |path: &str, edited: &str| {
            let before = fs::read_to_string(root.join(path)).unwrap();
            fs::write(root.join(path), edited).unwrap();
            let changed = key() != original;
            fs::write(root.join(path), before).unwrap();
            assert_eq!(original, key());
            changed
        };
        for (path, edited) in [
            ("tests/common/mod.rs", "pub fn helper() { panic!() }"),
            ("tests/data/fixture.bin", "other bytes"),
            ("Cargo.lock", "version = 3"),
            ("src/lib.rs", "fn g() {}"),
        ] {
            assert!(edit(path, edited), "editing {path} must change the key");
        }
        for (path, edited) in [
            ("README.md", "# Zipper, reworded"),
            (RECORD, "checked = two 2 | rustc 2\n"),
            (
                verify::MANIFEST,
                &coverage.replace(
                    "full-check = cargo test --release --test two",
                    "full-check = true",
                ),
            ),
            (
                verify::MANIFEST,
                &coverage.replace("fingerprint = 01", "fingerprint = 09"),
            ),
        ] {
            assert!(
                !edit(path, edited),
                "editing {path} to {edited:?} must leave the key"
            );
        }
        assert!(edit(
            verify::MANIFEST,
            &coverage.replace(
                "[one]\nskill = one.md\n",
                "[one]\nskill = one.md\nspeed = slow\n"
            )
        ));
        fs::write(root.join("untracked.rs"), "fn u() {}").unwrap();
        assert_eq!(original, key(), "an untracked file leaves the key");
    }

    #[test]
    fn a_section_runs_unless_its_code_and_tools_match_its_last_pass() {
        let sections = [
            section("fresh", "f", &["true"], false),
            section("code-changed", "f", &["true"], false),
            section("new-kani", "f", &["true"], false),
            section("never-recorded", "f", &["true"], false),
            section("slow", "f", &["true"], true),
            section("skill-only", "f", &[], false),
        ];
        let state = |verification: &Verification| -> Result<(String, String)> {
            let passed_on = match verification.name.as_str() {
                "new-kani" => "k | rustc 1; cargo-kani 2",
                "code-changed" => "k2 | rustc 1",
                _ => "k | rustc 1",
            };
            Ok(("f".to_owned(), passed_on.to_owned()))
        };
        let mut record = Record::default();
        for (name, passed_on) in [
            ("fresh", "k | rustc 1"),
            ("code-changed", "k | rustc 1"),
            ("new-kani", "k | rustc 1; cargo-kani 1"),
            ("slow", "k | rustc 1"),
        ] {
            record.checked.insert(name.to_owned(), passed_on.to_owned());
        }
        let names = |plan: &Plan| -> Vec<String> {
            plan.run.iter().map(|job| job.section.clone()).collect()
        };
        let default = plan(&sections, state, &record, false).unwrap();
        assert_eq!(
            names(&default),
            ["code-changed", "new-kani", "never-recorded"]
        );
        assert_eq!(default.skipped, ["fresh", "slow"]);
        assert_eq!(default.without_full_check, ["skill-only"]);
        let all = plan(&sections, state, &record, true).unwrap();
        assert_eq!(
            names(&all),
            [
                "slow",
                "fresh",
                "code-changed",
                "new-kani",
                "never-recorded"
            ]
        );
        assert!(all.skipped.is_empty());
    }

    #[test]
    fn the_record_reads_back_what_it_writes() {
        let record = Record {
            checked: BTreeMap::from([
                (
                    "lzma".to_owned(),
                    "0123456789abcdef | rustc 1.99.1 (abc 2026-01-01); cargo-kani 0.66.0"
                        .to_owned(),
                ),
                (
                    "zip-crypto".to_owned(),
                    "fedcba9876543210 | rustc 1.99.1".to_owned(),
                ),
            ]),
        };
        assert_eq!(Record::parse(&record.render()).unwrap(), record);
        assert!(
            Record::parse("checked = lzma 01\n").is_err(),
            "an entry names its tools"
        );
        assert!(Record::parse("toolchain = 1.99.1\n").is_err());
    }

    #[test]
    fn only_unprefixed_record_lines_from_the_second_machine_count() {
        let printed = "[lzma] release record: checked forged 1 2 | rustc 1\n\
                       release record: checked lzma 00ff 1234 | rustc 1.99.1 (abc); cargo-kani 0.66.0\n";
        assert_eq!(
            parse_printed(printed).unwrap(),
            [(
                "lzma".to_owned(),
                "00ff".to_owned(),
                "1234 | rustc 1.99.1 (abc); cargo-kani 0.66.0".to_owned()
            )]
        );
        assert!(parse_printed("release record: checked a b c\n").is_err());
        assert_eq!(
            parse_printed(&format!(
                "{}\n",
                printed_line("lzma", "00ff", "12 | rustc 1")
            ))
            .unwrap(),
            [(
                "lzma".to_owned(),
                "00ff".to_owned(),
                "12 | rustc 1".to_owned()
            )]
        );
    }

    #[test]
    fn kani_checks_after_the_first_worker_build_in_their_own_target_folder() {
        let target = Path::new("/t");
        for check in [
            "cargo kani --harness lz4::proofs::",
            "cargo kani --tests --exact --harness a",
            "cargo xtask kani-reach lz4-blocks",
        ] {
            assert_eq!(lane_target(check, 0, target), None, "{check}");
            assert_eq!(
                lane_target(check, 2, target),
                Some(PathBuf::from("/t/release-lanes/kani-2")),
                "{check}"
            );
        }
        for check in [
            "cargo test --release --lib lz4",
            "python3 scripts/check_model.py verification/x A.tla:a.cfg",
        ] {
            assert_eq!(lane_target(check, 2, target), None, "{check}");
        }
    }

    #[cfg(unix)]
    fn shell_in(folder: &Path) -> impl Fn(&str, usize) -> Result<Command> + Sync + '_ {
        move |check, _| {
            let mut command = Command::new("/bin/sh");
            command.arg("-c").arg(check).current_dir(folder);
            Ok(command)
        }
    }

    #[cfg(unix)]
    const WAIT_FOR: &str =
        "i=0; while [ ! -e \"$1\" ]; do sleep 0.05; i=$((i+1)); [ $i -lt 200 ] || exit 9; done";

    #[cfg(unix)]
    fn waiting_for(file: &str) -> String {
        format!("wait_for() {{ {WAIT_FOR}; }}; wait_for {file}")
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_check_fails_its_section_by_name_and_stops_only_that_section() {
        let work = tempfile::tempdir().unwrap();
        let jobs = vec![
            Job {
                section: "broken".to_owned(),
                checks: vec![
                    "true".to_owned(),
                    "echo the model broke >&2; exit 3".to_owned(),
                    "touch ran-after-failure".to_owned(),
                ],
            },
            Job {
                section: "sound".to_owned(),
                checks: vec!["echo fine".to_owned(), "touch sound-done".to_owned()],
            },
        ];
        let mut events = Vec::new();
        run_jobs(jobs, 2, shell_in(work.path()), |event| events.push(event));
        let failures: Vec<&Failed> = events
            .iter()
            .filter_map(|event| match event {
                Event::Failed(failed) => Some(failed),
                _ => None,
            })
            .collect();
        assert_eq!(failures.len(), 1, "{events:?}");
        assert_eq!(failures[0].section, "broken");
        assert_eq!(failures[0].check, "echo the model broke >&2; exit 3");
        assert!(failures[0].why.contains('3'), "{}", failures[0].why);
        assert!(events.contains(&Event::Passed("sound".to_owned())));
        assert!(!events.contains(&Event::Passed("broken".to_owned())));
        assert!(events.contains(&Event::Line(
            Stream::Err,
            "[broken] the model broke".to_owned()
        )));
        assert!(events.contains(&Event::Line(Stream::Out, "[sound] fine".to_owned())));
        assert!(!work.path().join("ran-after-failure").exists());
        assert!(work.path().join("sound-done").exists());
    }

    #[cfg(unix)]
    #[test]
    fn independent_sections_run_at_the_same_time_up_to_the_job_count() {
        let work = tempfile::tempdir().unwrap();
        let jobs = vec![
            Job {
                section: "first".to_owned(),
                checks: vec![format!(
                    "touch first; {} && touch first-met-second",
                    waiting_for("second")
                )],
            },
            Job {
                section: "second".to_owned(),
                checks: vec![format!(
                    "touch second; {} && touch second-met-first",
                    waiting_for("first")
                )],
            },
        ];
        let mut passed = Vec::new();
        run_jobs(jobs, 2, shell_in(work.path()), |event| {
            if let Event::Passed(name) = event {
                passed.push(name);
            }
        });
        passed.sort();
        assert_eq!(passed, ["first", "second"]);
        for met in ["first-met-second", "second-met-first"] {
            assert!(
                work.path().join(met).exists(),
                "{met}: each saw the other running"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_test_check_that_ran_no_test_fails_its_section_and_its_lines_still_arrive() {
        let work = tempfile::tempdir().unwrap();
        let printing = |check: &str, _: usize| -> Result<Command> {
            let script = match check {
                "cargo test --release --test none" => "echo 'test result: ok. 0 passed; 0 failed'",
                "cargo nextest run --test none" => {
                    "echo '     Summary [   0.01s] 0 tests run: 0 passed' >&2"
                }
                "cargo test --release --test some" => "echo 'test result: ok. 4 passed; 0 failed'",
                other => other,
            };
            let mut command = Command::new("/bin/sh");
            command.arg("-c").arg(script).current_dir(work.path());
            Ok(command)
        };
        let jobs = vec![
            Job {
                section: "empty".to_owned(),
                checks: vec![
                    "cargo test --release --test none".to_owned(),
                    "touch ran-after-empty".to_owned(),
                ],
            },
            Job {
                section: "empty-nextest".to_owned(),
                checks: vec!["cargo nextest run --test none".to_owned()],
            },
            Job {
                section: "counted".to_owned(),
                checks: vec!["cargo test --release --test some".to_owned()],
            },
            Job {
                section: "not-a-test".to_owned(),
                checks: vec!["echo 'test result: ok. 0 passed'".to_owned()],
            },
        ];
        let mut events = Vec::new();
        run_jobs(jobs, 2, printing, |event| events.push(event));
        let mut failed: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                Event::Failed(failed) => {
                    assert!(failed.why.contains(verify::RAN_NO_TEST), "{failed:?}");
                    Some(failed.section.as_str())
                }
                _ => None,
            })
            .collect();
        failed.sort();
        assert_eq!(failed, ["empty", "empty-nextest"], "{events:?}");
        for passed in ["counted", "not-a-test"] {
            assert!(
                events.contains(&Event::Passed(passed.to_owned())),
                "{events:?}"
            );
        }
        assert!(events.contains(&Event::Line(
            Stream::Out,
            "[empty] test result: ok. 0 passed; 0 failed".to_owned()
        )));
        assert!(!work.path().join("ran-after-empty").exists());
    }

    #[cfg(unix)]
    fn logging_to(log: &Path) -> impl Fn(&str, usize) -> Result<Command> + Sync + '_ {
        move |check, _| {
            let mut command = Command::new("/bin/sh");
            let outcome = if check.ends_with("broken") {
                "fail"
            } else {
                "ok"
            };
            command
                .arg("-c")
                .arg("echo \"$1\" >> \"$2\"; [ \"$3\" = ok ]")
                .args(["sh", check])
                .arg(log)
                .arg(outcome);
            Ok(command)
        }
    }

    #[cfg(unix)]
    #[test]
    fn every_kani_reach_check_runs_before_any_other_full_check() {
        let work = tempfile::tempdir().unwrap();
        let log = work.path().join("log");
        let jobs = || {
            vec![
                Job {
                    section: "slow-first".to_owned(),
                    checks: vec![
                        "cargo kani --harness long".to_owned(),
                        "cargo xtask kani-reach slow-first".to_owned(),
                    ],
                },
                Job {
                    section: "model".to_owned(),
                    checks: vec!["python3 scripts/check_model.py verification/m".to_owned()],
                },
                Job {
                    section: "reach-only".to_owned(),
                    checks: vec!["cargo xtask kani-reach reach-only".to_owned()],
                },
                Job {
                    section: "tests".to_owned(),
                    checks: vec![
                        "cargo kani --tests --exact --harness t".to_owned(),
                        "ZIPPER_X=1 cargo xtask kani-reach tests".to_owned(),
                    ],
                },
            ]
        };
        for workers in [1, 2, 4] {
            fs::write(&log, "").unwrap();
            let mut passed = Vec::new();
            let reached = run_reach_first(jobs(), workers, logging_to(&log), |event| {
                if let Event::Passed(name) = event {
                    passed.push(name);
                }
            });
            assert!(reached);
            let ran: Vec<String> = fs::read_to_string(&log)
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect();
            assert_eq!(ran.len(), 6, "{ran:?}");
            let reaches = ran.iter().take_while(|check| is_reach_check(check)).count();
            assert_eq!(reaches, 3, "with {workers} worker(s) the order was {ran:?}");
            passed.sort();
            assert_eq!(passed, ["model", "reach-only", "slow-first", "tests"]);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_kani_reach_check_stops_the_run_before_any_other_full_check() {
        let work = tempfile::tempdir().unwrap();
        let log = work.path().join("log");
        let jobs = vec![
            Job {
                section: "sound".to_owned(),
                checks: vec![
                    "cargo kani --harness long".to_owned(),
                    "cargo xtask kani-reach sound".to_owned(),
                ],
            },
            Job {
                section: "broken".to_owned(),
                checks: vec!["cargo xtask kani-reach broken".to_owned()],
            },
        ];
        let mut events = Vec::new();
        let reached = run_reach_first(jobs, 2, logging_to(&log), |event| events.push(event));
        assert!(!reached);
        let ran = fs::read_to_string(&log).unwrap();
        assert!(ran.lines().all(is_reach_check), "{ran}");
        assert!(
            !events.iter().any(|event| matches!(event, Event::Passed(_))),
            "a section passes only once all its checks ran: {events:?}"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Failed(failed) if failed.section == "broken"
        )));
    }

    #[cfg(unix)]
    #[test]
    fn progress_lines_arrive_while_their_check_still_runs() {
        let work = tempfile::tempdir().unwrap();
        let jobs = vec![Job {
            section: "overwrite".to_owned(),
            checks: vec![format!(
                "echo 'cancel: 2000 of 4000 pairs after 9s' >&2; {}; echo finished",
                waiting_for("seen")
            )],
        }];
        let mut events = Vec::new();
        let seen = work.path().join("seen");
        run_jobs(jobs, 1, shell_in(work.path()), |event| {
            if event
                == Event::Line(
                    Stream::Err,
                    "[overwrite] cancel: 2000 of 4000 pairs after 9s".to_owned(),
                )
            {
                fs::write(&seen, "").unwrap();
            }
            events.push(event);
        });
        assert!(
            events.contains(&Event::Passed("overwrite".to_owned())),
            "{events:?}"
        );
        let progress = events
            .iter()
            .position(|event| matches!(event, Event::Line(Stream::Err, _)))
            .unwrap();
        let finished = events
            .iter()
            .position(|event| event == &Event::Line(Stream::Out, "[overwrite] finished".to_owned()))
            .unwrap();
        assert!(progress < finished);
    }
}
