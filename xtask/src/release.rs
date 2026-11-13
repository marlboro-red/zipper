use crate::verify::{self, Verification};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
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
# toolchain, rust-version and kani name what the last passing `release-checks --all` ran on.
# Each `checked` line names a section and the key of the code and check inputs its full checks last passed on.
";
const PRINTED: &str = "release record: ";
pub(crate) const DEFAULT_JOBS: usize = 3;

pub(crate) struct Options {
    pub(crate) all: bool,
    pub(crate) jobs: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Toolchain {
    pub(crate) channel: String,
    pub(crate) rust_version: String,
    pub(crate) kani: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) toolchain: Option<Toolchain>,
    pub(crate) checked: BTreeMap<String, String>,
}

impl Record {
    fn parse(text: &str) -> Result<Self> {
        let mut fields: BTreeMap<&str, String> = BTreeMap::new();
        let mut checked = BTreeMap::new();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line
                .split_once(" = ")
                .with_context(|| format!("{RECORD}:{}: expected `key = value`", index + 1))?;
            match key {
                "toolchain" | "rust-version" | "kani" => {
                    fields.insert(key, value.to_owned());
                }
                "checked" => {
                    let (name, hash) = value.split_once(' ').with_context(|| {
                        format!("{RECORD}:{}: expected `checked = NAME KEY`", index + 1)
                    })?;
                    checked.insert(name.to_owned(), hash.to_owned());
                }
                other => bail!("{RECORD}:{}: unknown key {other:?}", index + 1),
            }
        }
        let toolchain = match (
            fields.remove("toolchain"),
            fields.remove("rust-version"),
            fields.remove("kani"),
        ) {
            (Some(channel), Some(rust_version), Some(kani)) => Some(Toolchain {
                channel,
                rust_version,
                kani,
            }),
            _ => None,
        };
        Ok(Self { toolchain, checked })
    }

    fn render(&self) -> String {
        let mut text = RECORD_HEADER.to_owned();
        if let Some(toolchain) = &self.toolchain {
            text.push_str(&format!(
                "toolchain = {}\nrust-version = {}\nkani = {}\n",
                toolchain.channel, toolchain.rust_version, toolchain.kani
            ));
        }
        for (name, key) in &self.checked {
            text.push_str(&format!("checked = {name} {key}\n"));
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
    fs::write(root.join(RECORD), record.render()).with_context(|| format!("cannot write {RECORD}"))
}

pub(crate) fn record_checked(root: &Path, name: &str, key: &str) -> Result<()> {
    let mut record = read_record(root)?;
    record.checked.insert(name.to_owned(), key.to_owned());
    write_record(root, &record)
}

fn quoted_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let value = line
            .trim()
            .strip_prefix(key)?
            .trim_start()
            .strip_prefix('=')?;
        Some(value.trim().trim_matches('"').to_owned())
    })
}

fn current_toolchain(root: &Path) -> Result<Toolchain> {
    let channel = quoted_value(
        &verify::read_source(root, "rust-toolchain.toml")?,
        "channel",
    )
    .context("rust-toolchain.toml names no channel")?;
    let rust_version = quoted_value(&verify::read_source(root, "Cargo.toml")?, "rust-version")
        .context("Cargo.toml has no rust-version")?;
    let kani = Command::new("cargo")
        .args(["kani", "--version"])
        .current_dir(root)
        .output()
        .context("cannot run `cargo kani --version`")?;
    ensure!(
        kani.status.success(),
        "`cargo kani --version` exited with {}: {}",
        kani.status,
        String::from_utf8_lossy(&kani.stderr).trim()
    );
    Ok(Toolchain {
        channel,
        rust_version,
        kani: String::from_utf8(kani.stdout)?.trim().to_owned(),
    })
}

pub(crate) fn all_required_because(record: &Record, now: &Toolchain) -> Option<String> {
    let Some(last) = &record.toolchain else {
        return Some(format!("{RECORD} names no toolchain a full pass ran on"));
    };
    let changed: Vec<String> = [
        (
            "the channel in rust-toolchain.toml",
            &last.channel,
            &now.channel,
        ),
        (
            "rust-version in Cargo.toml",
            &last.rust_version,
            &now.rust_version,
        ),
        ("the Kani version", &last.kani, &now.kani),
    ]
    .into_iter()
    .filter(|(_, was, is)| was != is)
    .map(|(what, was, is)| format!("{what} is {is}, but the last full pass ran on {was}"))
    .collect();
    (!changed.is_empty()).then(|| changed.join("; "))
}

fn check_inputs(root: &Path, check: &str) -> Vec<String> {
    let words: Vec<&str> = check.split_whitespace().collect();
    let mut named = BTreeSet::new();
    for (index, word) in words.iter().enumerate() {
        let mut candidates = Vec::new();
        if index > 0 && words[index - 1] == "--test" {
            candidates.push(format!("tests/{word}.rs"));
            candidates.push(format!("tests/{word}"));
        }
        if !word.starts_with(['-', '/']) && !word.contains('=') {
            candidates.push((*word).to_owned());
        }
        for candidate in candidates {
            let path = root.join(&candidate);
            if path.is_file() {
                named.insert(candidate);
            } else if path.is_dir() {
                for entry in walkdir::WalkDir::new(&path).sort_by_file_name() {
                    let Ok(entry) = entry else { continue };
                    if entry.file_type().is_file()
                        && let Ok(relative) = entry.path().strip_prefix(root)
                    {
                        named.insert(relative.to_string_lossy().replace('\\', "/"));
                    }
                }
            }
        }
    }
    named.into_iter().collect()
}

pub(crate) fn checked_key(
    root: &Path,
    verification: &Verification,
    fingerprint: &str,
) -> Result<String> {
    let mut hash = verify::fnv1a(0xcbf2_9ce4_8422_2325, fingerprint.bytes());
    for check in &verification.full_checks {
        hash = verify::fnv1a(hash, check.bytes().chain(*b"\n"));
        for path in check_inputs(root, check) {
            let bytes =
                fs::read(root.join(&path)).with_context(|| format!("cannot read {path}"))?;
            hash = verify::fnv1a(hash, path.bytes().chain([0]).chain(bytes));
        }
    }
    Ok(format!("{hash:016x}"))
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
    pub(crate) unsyncable: Vec<String>,
}

pub(crate) fn plan(
    sections: &[Verification],
    mut state: impl FnMut(&Verification) -> Result<(String, String)>,
    record: &Record,
    all: bool,
) -> Result<Plan> {
    let mut plan = Plan::default();
    let mut slow = Vec::new();
    let mut quick = Vec::new();
    for verification in sections {
        let (current, key) = state(verification)?;
        let fresh = current == verification.fingerprint;
        if verification.full_checks.is_empty() {
            if fresh {
                plan.without_full_check.push(verification.name.clone());
            } else {
                plan.unsyncable.push(verify::sync_instruction(verification));
            }
            continue;
        }
        if !all && fresh && record.checked.get(&verification.name) == Some(&key) {
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
        let status = thread::scope(|scope| {
            for (pipe, stream) in pipes {
                if let Some(pipe) = pipe {
                    let sender = sender.clone();
                    let section = job.section.as_str();
                    scope.spawn(move || forward_lines(pipe, stream, section, &sender));
                }
            }
            child.wait()
        });
        match status {
            Ok(status) if status.success() => {}
            Ok(status) => return failed(format!("exited with {status}")),
            Err(error) => return failed(format!("could not be waited for: {error}")),
        }
    }
    None
}

fn forward_lines(pipe: impl Read, stream: Stream, section: &str, sender: &mpsc::Sender<Event>) {
    for line in BufReader::new(pipe).split(b'\n') {
        let Ok(line) = line else { break };
        let text = String::from_utf8_lossy(&line);
        let text = text.strip_suffix('\r').unwrap_or(&text);
        if sender
            .send(Event::Line(stream, format!("[{section}] {text}")))
            .is_err()
        {
            break;
        }
    }
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
    let current = verify::fingerprint(root, verification)?;
    let key = checked_key(root, verification, &current)?;
    ensure!(
        planned.get(name) == Some(&(current.clone(), key.clone())),
        "the covered code or its check's inputs changed while the check ran; nothing was recorded"
    );
    verify::write_fingerprint(root, name, &current)?;
    record.checked.insert(name.to_owned(), key.clone());
    write_record(root, record)?;
    Ok(format!("checked {name} {current} {key}"))
}

pub(crate) fn run_full_checks(root: &Path, target: &Path, options: &Options) -> Result<Toolchain> {
    verify::fail_on_unready_tree(root)?;
    let toolchain = current_toolchain(root)?;
    let mut record = read_record(root)?;
    if !options.all
        && let Some(why) = all_required_because(&record, &toolchain)
    {
        bail!("{why}, so run `cargo xtask release-checks --all`");
    }
    let sections = verify::manifest(root)?;
    let mut planned = BTreeMap::new();
    let plan = plan(
        &sections,
        |verification| {
            let current = verify::fingerprint(root, verification)?;
            let key = checked_key(root, verification, &current)?;
            planned.insert(verification.name.clone(), (current.clone(), key.clone()));
            Ok((current, key))
        },
        &record,
        options.all,
    )?;
    ensure!(
        plan.unsyncable.is_empty(),
        "these sections changed and have no full check for release-checks to run:\n  {}",
        plan.unsyncable.join("\n  ")
    );
    for name in &plan.without_full_check {
        println!(
            "xtask: release check {name}: no full check; the gate's verify step keeps it in step with its skill"
        );
    }
    for name in &plan.skipped {
        println!(
            "xtask: release check {name}: skipped, since its full checks passed on this code and these check inputs"
        );
    }
    println!(
        "xtask: release checks: running {} section(s) on {} worker(s), skipping {}",
        plan.run.len(),
        options.jobs.max(1),
        plan.skipped.len()
    );
    let mut failed = Vec::new();
    run_jobs(
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
                Ok(line) => println!("{PRINTED}{line}"),
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
        failed.is_empty(),
        "release checks failed in {} section(s):\n  {}",
        failed.len(),
        failed.join("\n  ")
    );
    Ok(toolchain)
}

pub(crate) fn record_full_pass(root: &Path, toolchain: &Toolchain) -> Result<()> {
    let mut record = read_record(root)?;
    record.toolchain = Some(toolchain.clone());
    write_record(root, &record)?;
    println!("{PRINTED}toolchain = {}", toolchain.channel);
    println!("{PRINTED}rust-version = {}", toolchain.rust_version);
    println!("{PRINTED}kani = {}", toolchain.kani);
    Ok(())
}

type Printed = (Vec<(String, String, String)>, Option<Toolchain>);

fn parse_printed(printed: &str) -> Result<Printed> {
    let mut checked = Vec::new();
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    for line in printed.lines() {
        let Some(rest) = line.strip_prefix(PRINTED) else {
            continue;
        };
        if let Some(rest) = rest.strip_prefix("checked ") {
            let words: Vec<&str> = rest.split(' ').collect();
            let [name, fingerprint, key] = words[..] else {
                bail!("the second machine printed a malformed record line: {line}");
            };
            checked.push((name.to_owned(), fingerprint.to_owned(), key.to_owned()));
        } else if let Some((key, value)) = rest.split_once(" = ") {
            fields.insert(key.to_owned(), value.to_owned());
        }
    }
    let toolchain = match (
        fields.remove("toolchain"),
        fields.remove("rust-version"),
        fields.remove("kani"),
    ) {
        (Some(channel), Some(rust_version), Some(kani)) => Some(Toolchain {
            channel,
            rust_version,
            kani,
        }),
        _ => None,
    };
    Ok((checked, toolchain))
}

pub(crate) fn apply_printed_record(root: &Path, printed: &str, passed: bool) -> Result<String> {
    let (checked, toolchain) = parse_printed(printed)?;
    let sections = verify::manifest(root)?;
    let mut record = read_record(root)?;
    for (name, fingerprint, key) in &checked {
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
        let local_key = checked_key(root, verification, &local)?;
        ensure!(
            (&local, &local_key) == (fingerprint, key),
            "{name}: the second machine checked fingerprint {fingerprint} and key {key}, but this tree has {local} and {local_key}; nothing more was recorded"
        );
        verify::write_fingerprint(root, name, fingerprint)?;
        record.checked.insert(name.clone(), key.clone());
        write_record(root, &record)?;
    }
    let toolchain = toolchain.filter(|_| passed);
    if let Some(toolchain) = &toolchain {
        record.toolchain = Some(toolchain.clone());
        write_record(root, &record)?;
    }
    Ok(format!(
        "recorded {} section(s) checked on the second machine{}; commit {} and {}",
        checked.len(),
        if toolchain.is_some() {
            " and the toolchain of its full pass"
        } else {
            ""
        },
        verify::MANIFEST,
        RECORD
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
    fn a_section_runs_only_when_its_code_or_check_inputs_changed_since_its_checks_passed() {
        let sections = [
            section("fresh", "f1", &["true"], false),
            section("code-changed", "old", &["true"], false),
            section("inputs-changed", "f3", &["true"], false),
            section("never-recorded", "f4", &["true"], false),
            section("owed", "old", &["true"], true),
            section("skill-only", "f6", &[], false),
            section("skill-only-stale", "old", &[], false),
        ];
        let state = |verification: &Verification| -> Result<(String, String)> {
            let current = match verification.name.as_str() {
                "fresh" => "f1",
                "inputs-changed" => "f3",
                "never-recorded" => "f4",
                "skill-only" => "f6",
                _ => "new",
            };
            Ok((current.to_owned(), format!("key-{current}")))
        };
        let mut record = Record::default();
        for (name, key) in [
            ("fresh", "key-f1"),
            ("code-changed", "key-old"),
            ("inputs-changed", "key-before-the-model-changed"),
            ("owed", "key-old"),
        ] {
            record.checked.insert(name.to_owned(), key.to_owned());
        }
        let default = plan(&sections, state, &record, false).unwrap();
        let names = |plan: &Plan| -> Vec<String> {
            plan.run.iter().map(|job| job.section.clone()).collect()
        };
        assert_eq!(
            names(&default),
            ["owed", "code-changed", "inputs-changed", "never-recorded"]
        );
        assert_eq!(default.skipped, ["fresh"]);
        assert_eq!(default.without_full_check, ["skill-only"]);
        assert_eq!(default.unsyncable.len(), 1);
        assert!(default.unsyncable[0].starts_with("skill-only-stale: follow skill-only-stale.md"));
        let all = plan(&sections, state, &record, true).unwrap();
        assert_eq!(
            names(&all),
            [
                "owed",
                "fresh",
                "code-changed",
                "inputs-changed",
                "never-recorded"
            ]
        );
        assert!(all.skipped.is_empty());
    }

    #[test]
    fn the_checked_key_follows_the_code_the_check_text_and_the_files_a_check_names() {
        let work = tempfile::tempdir().unwrap();
        let root = work.path();
        for (path, text) in [
            ("verification/model/MC.tla", "---- MODULE MC ----"),
            ("verification/model/mc.cfg", "INVARIANT Safe"),
            ("scripts/check_model.py", "print()"),
            ("tests/exhaustive.rs", "#[test] fn t() {}"),
            ("src/unrelated.rs", "fn f() {}"),
        ] {
            fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            fs::write(root.join(path), text).unwrap();
        }
        let checks = [
            "python3 scripts/check_model.py verification/model MC.tla:mc.cfg",
            "cargo test --release --test exhaustive STRIDE=1 -- --nocapture",
        ];
        let verification = section("s", "fp", &checks, false);
        let key = || checked_key(root, &verification, "fp").unwrap();
        let original = key();
        assert_eq!(original, key());
        assert_ne!(original, checked_key(root, &verification, "other").unwrap());
        fs::write(root.join("src/unrelated.rs"), "fn g() {}").unwrap();
        assert_eq!(original, key(), "a file no check names leaves the key");
        for (path, edited) in [
            ("verification/model/mc.cfg", "INVARIANT Weaker"),
            ("tests/exhaustive.rs", "#[test] fn u() {}"),
            ("scripts/check_model.py", "print(1)"),
        ] {
            let before = fs::read_to_string(root.join(path)).unwrap();
            fs::write(root.join(path), edited).unwrap();
            assert_ne!(original, key(), "editing {path}");
            fs::write(root.join(path), before).unwrap();
            assert_eq!(original, key());
        }
        let restrided = section(
            "s",
            "fp",
            &[
                checks[0],
                "cargo test --release --test exhaustive STRIDE=9 -- --nocapture",
            ],
            false,
        );
        assert_ne!(original, checked_key(root, &restrided, "fp").unwrap());
    }

    #[test]
    fn a_new_toolchain_rust_version_or_kani_requires_rerunning_everything() {
        let toolchain = Toolchain {
            channel: "1.99.1".to_owned(),
            rust_version: "1.95".to_owned(),
            kani: "cargo-kani 0.66.0".to_owned(),
        };
        let mut record = Record::default();
        assert!(all_required_because(&record, &toolchain).is_some());
        record.toolchain = Some(toolchain.clone());
        assert_eq!(all_required_because(&record, &toolchain), None);
        for (changed, mention) in [
            (
                Toolchain {
                    channel: "1.100.0".to_owned(),
                    ..toolchain.clone()
                },
                "rust-toolchain.toml",
            ),
            (
                Toolchain {
                    rust_version: "1.96".to_owned(),
                    ..toolchain.clone()
                },
                "Cargo.toml",
            ),
            (
                Toolchain {
                    kani: "cargo-kani 0.67.0".to_owned(),
                    ..toolchain.clone()
                },
                "Kani",
            ),
        ] {
            let why = all_required_because(&record, &changed).expect("a change requires --all");
            assert!(why.contains(mention), "{why}");
        }
    }

    #[test]
    fn the_record_reads_back_what_it_writes() {
        let record = Record {
            toolchain: Some(Toolchain {
                channel: "1.99.1".to_owned(),
                rust_version: "1.95".to_owned(),
                kani: "cargo-kani 0.66.0".to_owned(),
            }),
            checked: BTreeMap::from([
                ("lzma".to_owned(), "0123456789abcdef".to_owned()),
                ("zip-crypto".to_owned(), "fedcba9876543210".to_owned()),
            ]),
        };
        assert_eq!(Record::parse(&record.render()).unwrap(), record);
        let partial = Record::parse("checked = lzma 01\nkani = cargo-kani 0.66.0\n").unwrap();
        assert_eq!(
            partial.toolchain, None,
            "a toolchain without every field is no full pass"
        );
    }

    #[test]
    fn only_unprefixed_record_lines_from_the_second_machine_count() {
        let printed = "[lzma] release record: checked forged 1 2\n\
                       release record: checked lzma 00ff 1234\n\
                       release record: toolchain = 1.99.1\n\
                       release record: rust-version = 1.95\n\
                       release record: kani = cargo-kani 0.66.0\n";
        let (checked, toolchain) = parse_printed(printed).unwrap();
        assert_eq!(
            checked,
            [("lzma".to_owned(), "00ff".to_owned(), "1234".to_owned())]
        );
        assert_eq!(toolchain.unwrap().kani, "cargo-kani 0.66.0");
        assert!(parse_printed("release record: checked a b\n").is_err());
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
