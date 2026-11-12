use anyhow::{Context, Result, bail, ensure};
use std::{fs, path::Path, process::Command};

pub(crate) const MANIFEST: &str = "verification/coverage.txt";
const UNCOVERED: &str = "verification/uncovered.txt";

pub(crate) struct Verification {
    pub(crate) name: String,
    skill: String,
    fingerprint: String,
    slow: bool,
    pub(crate) full_checks: Vec<String>,
    pub(crate) items: Vec<(String, String)>,
}

pub(crate) fn manifest(root: &Path) -> Result<Vec<Verification>> {
    let text = fs::read_to_string(root.join(MANIFEST))
        .with_context(|| format!("cannot read {MANIFEST}"))?;
    let mut all: Vec<Verification> = Vec::new();
    for (number, line) in text
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line))
    {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            all.push(Verification {
                name: name.to_owned(),
                skill: String::new(),
                fingerprint: String::new(),
                slow: false,
                full_checks: Vec::new(),
                items: Vec::new(),
            });
            continue;
        }
        let (key, value) = line
            .split_once(" = ")
            .with_context(|| format!("{MANIFEST}:{number}: expected `key = value`"))?;
        let entry = all
            .last_mut()
            .with_context(|| format!("{MANIFEST}:{number}: a key before any [name]"))?;
        match key {
            "skill" => entry.skill = value.to_owned(),
            "fingerprint" => entry.fingerprint = value.to_owned(),
            "speed" => {
                ensure!(
                    value == "slow",
                    "{MANIFEST}:{number}: `speed` takes only `slow`"
                );
                entry.slow = true;
            }
            "full-check" => entry.full_checks.push(value.to_owned()),
            "item" => {
                let (path, opening) = value.split_once(" | ").with_context(|| {
                    format!("{MANIFEST}:{number}: expected `path | opening line`")
                })?;
                entry.items.push((path.to_owned(), opening.to_owned()));
            }
            other => bail!("{MANIFEST}:{number}: unknown key {other:?}"),
        }
    }
    Ok(all)
}

fn item_spans_starting_with<'a>(source: &'a str, opening: &str) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        if line.starts_with(opening) {
            let end = offset + end_of_braced_body_or_semicolon(&source[offset..]);
            found.push(&source[start_of_kani_attributes_above(source, offset)..end]);
        }
        offset += line.len();
    }
    found
}

fn start_of_kani_attributes_above(source: &str, item: usize) -> usize {
    let mut start = item;
    let mut kani = item;
    for line in source[..item].split_inclusive('\n').rev() {
        let code = line.trim();
        if code.is_empty() || code.ends_with(['{', '}', ';']) && !code.starts_with("#[") {
            break;
        }
        start -= line.len();
        if code.starts_with("#[kani::") {
            kani = start;
        }
    }
    if kani < item { start } else { item }
}

pub(crate) fn end_of_braced_body_or_semicolon(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut brackets = 0usize;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'"' => {
                index += 1;
                while index < bytes.len() && bytes[index] != b'"' {
                    index += if bytes[index] == b'\\' { 2 } else { 1 };
                }
            }
            b'\'' if bytes.get(index + 2) == Some(&b'\'') => index += 2,
            b'\'' if bytes.get(index + 1) == Some(&b'\\') => {
                while index + 1 < bytes.len() && bytes[index + 1] != b'\'' {
                    index += 1;
                }
                index += 1;
            }
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return index + 1;
                }
            }
            b'[' | b'(' if depth == 0 => brackets += 1,
            b']' | b')' if depth == 0 => brackets = brackets.saturating_sub(1),
            b';' if depth == 0 && brackets == 0 => {
                return index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    bytes.len()
}

// EXTERNAL(Git on Windows): checkouts can turn LF into CRLF.
pub(crate) fn fnv1a(mut hash: u64, bytes: impl Iterator<Item = u8>) -> u64 {
    for byte in bytes.filter(|&byte| byte != b'\r') {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn fingerprint(root: &Path, verification: &Verification) -> Result<String> {
    fingerprint_of(verification, |path| {
        fs::read_to_string(root.join(path)).with_context(|| format!("cannot read {path}"))
    })
}

fn fingerprint_of(
    verification: &Verification,
    source_of: impl Fn(&str) -> Result<String>,
) -> Result<String> {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (path, opening) in &verification.items {
        let source = source_of(path)?;
        let texts = item_spans_starting_with(&source, opening);
        ensure!(
            !texts.is_empty(),
            "{}: no line in {path} starts with {opening:?}; update {MANIFEST} and the model",
            verification.name
        );
        for text in texts {
            hash = fnv1a(hash, path.bytes().chain(text.bytes()));
        }
    }
    Ok(format!("{hash:016x}"))
}

const SHARED_FIELD_CHECK: [(&str, &str); 3] = [
    ("src/lib.rs", "pub(crate) fn field_check("),
    ("src/lib.rs", "pub(crate) struct BadField("),
    ("src/lib.rs", "pub(crate) type Field<"),
];

fn names_the_field_check(text: &str) -> bool {
    ["field_check(", "Field<"].iter().any(|name| {
        text.match_indices(name).any(|(at, _)| {
            !text[..at]
                .chars()
                .next_back()
                .is_some_and(|before| before.is_alphanumeric() || before == '_')
        })
    })
}

fn missing_shared_items(
    verification: &Verification,
    source_of: impl Fn(&str) -> Result<String>,
) -> Result<Vec<String>> {
    let listed = |(path, opening): &(&str, &str)| {
        verification
            .items
            .iter()
            .any(|(have_path, have)| have_path == path && have == opening)
    };
    let mut uses = false;
    for (path, opening) in &verification.items {
        if SHARED_FIELD_CHECK
            .iter()
            .any(|(p, o)| p == path && o == opening)
        {
            continue;
        }
        let source = source_of(path)?;
        uses |= item_spans_starting_with(&source, opening)
            .iter()
            .any(|text| names_the_field_check(text));
    }
    Ok(if uses {
        SHARED_FIELD_CHECK
            .iter()
            .filter(|item| !listed(item))
            .map(|(path, opening)| format!("item = {path} | {opening}"))
            .collect()
    } else {
        Vec::new()
    })
}

fn fail_on_interrupted_plants(root: &Path) -> Result<()> {
    let journal = root.join("target/plant-journal");
    let mut left = Vec::new();
    if journal.is_dir() {
        for entry in fs::read_dir(&journal)? {
            left.push(entry?.path().display().to_string());
        }
    }
    ensure!(
        left.is_empty(),
        "an interrupted scripts/plant.py may have left a break in the code ({}); run `python3 scripts/plant.py --restore`",
        left.join(", ")
    );
    Ok(())
}

pub fn fail_on_stale_fingerprints(root: &Path) -> Result<()> {
    fail_on_stale(root, Owed::Allowed)
}

pub fn fail_on_owed_slow_checks(root: &Path) -> Result<()> {
    fail_on_stale(root, Owed::Refused)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Owed {
    Allowed,
    Refused,
}

fn fail_on_stale(root: &Path, owed: Owed) -> Result<()> {
    fail_on_interrupted_plants(root)?;
    fail_on_unclassified_sources(root)?;
    crate::kani::fail_on_harnesses_not_run_as_listed(root)?;
    let mut stale = Vec::new();
    for verification in manifest(root)? {
        let missing = missing_shared_items(&verification, |path| {
            fs::read_to_string(root.join(path)).with_context(|| format!("cannot read {path}"))
        })?;
        if !missing.is_empty() {
            stale.push(format!(
                "{}: its items call the shared field check, so add to {MANIFEST}: {}",
                verification.name,
                missing.join(", ")
            ));
        }
        let current = fingerprint(root, &verification)?;
        if current == verification.fingerprint {
            continue;
        }
        let instruction = format!(
            "{}: follow {}, then run `cargo xtask verify-sync {}`",
            verification.name, verification.skill, verification.name
        );
        if verification.slow && owed == Owed::Allowed {
            eprintln!("owed before a release or milestone close: {instruction}");
        } else {
            stale.push(instruction);
        }
    }
    ensure!(
        stale.is_empty(),
        "code covered by a formal check changed since the check last passed:\n  {}",
        stale.join("\n  ")
    );
    Ok(())
}

fn run_full_check(root: &Path, check: &str) -> Result<bool> {
    let mut words = check.split_whitespace();
    let mut command = Command::new(words.next().context("empty full-check")?);
    for word in words {
        match word.split_once('=') {
            Some((key, value)) if key.chars().all(|c| c.is_ascii_uppercase() || c == '_') => {
                command.env(key, value);
            }
            _ => {
                command.arg(word);
            }
        }
    }
    Ok(command.current_dir(root).status()?.success())
}

pub fn run_every_full_check(root: &Path) -> Result<()> {
    let mut failed = Vec::new();
    for verification in manifest(root)? {
        if verification.full_checks.is_empty() {
            println!(
                "xtask: release check {}: no full check; the gate's verify step keeps it in step with {}",
                verification.name, verification.skill
            );
        }
        for check in &verification.full_checks {
            println!("xtask: release check {}: {check}", verification.name);
            if !run_full_check(root, check)? {
                failed.push(verification.name.clone());
            }
        }
    }
    ensure!(
        failed.is_empty(),
        "release checks failed: {}",
        failed.join(", ")
    );
    Ok(())
}

pub fn record_checked_fingerprint(root: &Path, name: &str) -> Result<()> {
    let all = manifest(root)?;
    let verification = all
        .iter()
        .find(|verification| verification.name == name)
        .with_context(|| format!("no verification named {name:?} in {MANIFEST}"))?;
    let checked = fingerprint(root, verification)?;
    for check in &verification.full_checks {
        ensure!(
            run_full_check(root, check)?,
            "{name}: the full check `{check}` failed; nothing was synced"
        );
    }
    if verification.full_checks.is_empty() {
        println!(
            "{name}: recording the current code as checked; {} must have passed on it",
            verification.skill
        );
    }
    let current = fingerprint(root, verification)?;
    ensure!(
        current == checked,
        "{name}: the covered code changed while the check ran; nothing was synced"
    );
    let text = fs::read_to_string(root.join(MANIFEST))?;
    fs::write(root.join(MANIFEST), with_fingerprint(&text, name, &current))?;
    println!("{name}: synced at fingerprint {current}");
    Ok(())
}

fn with_fingerprint(text: &str, name: &str, current: &str) -> String {
    let header = format!("[{name}]");
    let has_line = section_has_fingerprint(text, &header);
    let mut in_section = false;
    let mut updated = String::new();
    for line in text.lines() {
        if line.starts_with('[') {
            in_section = line == header;
        }
        if in_section && line.starts_with("fingerprint = ") {
            updated.push_str(&format!("fingerprint = {current}\n"));
            continue;
        }
        updated.push_str(line);
        updated.push('\n');
        if line == header && !has_line {
            updated.push_str(&format!("fingerprint = {current}\n"));
        }
    }
    updated
}

fn section_has_fingerprint(text: &str, header: &str) -> bool {
    text.lines()
        .skip_while(|line| *line != header)
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .any(|line| line.starts_with("fingerprint = "))
}

fn uncovered(root: &Path) -> Result<Vec<String>> {
    let text = fs::read_to_string(root.join(UNCOVERED))
        .with_context(|| format!("cannot read {UNCOVERED}"))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|(index, line)| {
            let (path, reason) = line
                .split_once(" | ")
                .with_context(|| format!("{UNCOVERED}:{}: expected `path | reason`", index + 1))?;
            ensure!(
                !reason.trim().is_empty(),
                "{UNCOVERED}:{}: {path} needs a reason",
                index + 1
            );
            Ok(path.to_owned())
        })
        .collect()
}

pub(crate) fn read_source(root: &Path, path: &str) -> Result<String> {
    fs::read_to_string(root.join(path)).with_context(|| format!("cannot read {path}"))
}

pub(crate) fn sources(root: &Path) -> Result<Vec<String>> {
    let mut found = Vec::new();
    let mut folders = vec![root.join("src")];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder)? {
            let path = entry?.path();
            if path.is_dir() {
                folders.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let relative = path
                    .strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/");
                found.push(relative);
            }
        }
    }
    found.sort();
    Ok(found)
}

fn unclassified(sources: &[String], covered: &[String], uncovered: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    for source in sources {
        match (covered.contains(source), uncovered.contains(source)) {
            (false, false) => problems.push(format!(
                "{source} has no formal check in {MANIFEST} and no reason in {UNCOVERED}"
            )),
            (true, true) => problems.push(format!(
                "{source} is covered in {MANIFEST}, so remove its line from {UNCOVERED}"
            )),
            _ => {}
        }
    }
    for path in uncovered {
        if !sources.contains(path) {
            problems.push(format!("{UNCOVERED} lists {path}, which does not exist"));
        }
    }
    problems
}

fn fail_on_unclassified_sources(root: &Path) -> Result<()> {
    let covered: Vec<String> = manifest(root)?
        .into_iter()
        .flat_map(|verification| verification.items.into_iter().map(|(path, _)| path))
        .collect();
    let problems = unclassified(&sources(root)?, &covered, &uncovered(root)?);
    ensure!(problems.is_empty(), "{}", problems.join("\n"));
    Ok(())
}

const REMOTE_SETTINGS: [&str; 3] = ["zipper.remoteHost", "zipper.remoteGit", "zipper.remoteDir"];

fn local_git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").current_dir(root).args(args).output()?;
    ensure!(
        output.status.success(),
        "git {} exited with {}: {}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn remote_settings(root: &Path) -> Result<[String; 3]> {
    let read = |key: &str| {
        local_git(root, &["config", "--get", key]).with_context(|| {
            format!(
                "set `git config {key}` first. `--remote` reads {REMOTE_SETTINGS:?}: \
                 the ssh host, the git remote its checkout fetches from as `origin`, and that checkout's folder"
            )
        })
    };
    Ok([
        read(REMOTE_SETTINGS[0])?,
        read(REMOTE_SETTINGS[1])?,
        read(REMOTE_SETTINGS[2])?,
    ])
}

fn shell_quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

const REMOTE_TOOLS_MISSING: i32 = 3;

fn remote_preflight() -> String {
    format!(
        "missing=\"\"\n\
         for tool in 7zz unsquashfs; do command -v $tool >/dev/null 2>&1 || missing=\"$missing $tool\"; done\n\
         [ -x \"${{STATECHART:-$HOME/.claude/skills/statechart/scripts/statechart}}\" ] || missing=\"$missing statechart\"\n\
         if [ -n \"$missing\" ]; then\n\
         echo \"the second machine lacks:$missing; install them before a --remote run, or its gate skips or fails checks\" >&2\n\
         exit {REMOTE_TOOLS_MISSING}\n\
         fi\n"
    )
}

fn remote_lock(base: &str) -> String {
    format!(
        "until mkdir {base}/target/remote-run.lock 2>/dev/null; do\n\
         find {base}/target/remote-run.lock -maxdepth 0 -mmin +180 -exec rmdir {{}} \\; 2>/dev/null\n\
         sleep 5\n\
         done\n\
         trap 'rmdir {base}/target/remote-run.lock' EXIT\n"
    )
}

fn remote_script(dir: &str, branch: &str, head: &str, xtask: &[&str]) -> String {
    let [dir, branch, head] = [dir, branch, head].map(shell_quoted);
    let xtask: Vec<String> = xtask.iter().map(|word| shell_quoted(word)).collect();
    let xtask = xtask.join(" ");
    format!(
        "set -e\n\
         cd {dir}\n\
         base=$PWD\n\
         {preflight}\
         mkdir -p target\n\
         {lock}\
         git fetch -q origin \"refs/heads/\"{branch}\n\
         mkdir -p target\n\
         run=$(mktemp -d \"$base/target/remote-sync.XXXXXX\")\n\
         git worktree add -q --detach \"$run/tree\" {head}\n\
         result=0\n\
         (cd \"$run/tree\" && PATH=\"$base/target/tools:$PATH\" \
         CARGO_TARGET_DIR=\"$base/target/remote-sync-build\" cargo xtask {xtask}) || result=$?\n\
         git -C \"$run/tree\" checkout -q -- .\n\
         git worktree remove \"$run/tree\"\n\
         rmdir \"$run\"\n\
         exit $result\n",
        preflight = remote_preflight(),
        lock = remote_lock("\"$base\""),
    )
}

fn ensure_committed(root: &Path) -> Result<()> {
    ensure!(
        local_git(root, &["status", "--porcelain"])?.is_empty(),
        "commit every change first, since the second machine checks the committed HEAD"
    );
    Ok(())
}

fn run_at_head_remotely(
    root: &Path,
    head: &str,
    label: &str,
    xtask: &[&str],
) -> Result<std::process::Output> {
    let [host, remote, dir] = remote_settings(root)?;
    let branch = format!("remote-sync/{}-{label}", &head[..12]);
    local_git(
        root,
        &[
            "push",
            "-q",
            "--no-verify",
            "--force",
            &remote,
            &format!("{head}:refs/heads/{branch}"),
        ],
    )?;
    let run = streamed(
        Command::new("ssh")
            .args([host.as_str(), "zsh", "-lc"])
            .arg(shell_quoted(&remote_script(&dir, &branch, head, xtask))),
    )?;
    let _ = local_git(
        root,
        &["push", "-q", "--no-verify", &remote, "--delete", &branch],
    );
    Ok(run)
}

fn streamed(command: &mut Command) -> Result<std::process::Output> {
    use std::io::{BufRead, Write};
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;
    let mut stdout = Vec::new();
    for line in std::io::BufReader::new(child.stdout.take().context("no stdout pipe")?).split(b'\n')
    {
        let mut line = line?;
        line.push(b'\n');
        std::io::stdout().write_all(&line)?;
        stdout.extend_from_slice(&line);
    }
    Ok(std::process::Output {
        status: child.wait()?,
        stdout,
        stderr: Vec::new(),
    })
}

pub fn gate_remotely(root: &Path, kind: &str) -> Result<()> {
    ensure_committed(root)?;
    let head = local_git(root, &["rev-parse", "HEAD"])?;
    let run = run_at_head_remotely(root, &head, "gate", &["gate", kind])?;
    ensure!(
        run.status.success(),
        "gate {kind} on the second machine at {} exited with {}",
        &head[..12],
        run.status
    );
    println!("gate {kind} passed on a second machine at {}", &head[..12]);
    Ok(())
}

pub fn record_remotely_checked_fingerprint(root: &Path, name: &str) -> Result<()> {
    let all = manifest(root)?;
    let verification = all
        .iter()
        .find(|verification| verification.name == name)
        .with_context(|| format!("no verification named {name:?} in {MANIFEST}"))?;
    ensure_committed(root)?;
    let head = local_git(root, &["rev-parse", "HEAD"])?;
    let checked = fingerprint(root, verification)?;
    let run = run_at_head_remotely(root, &head, name, &["verify-sync", name])?;
    let printed = String::from_utf8_lossy(&run.stdout);
    ensure!(
        run.status.success(),
        "{name}: the check on the second machine exited with {}; nothing was synced",
        run.status
    );
    let reported = printed
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{name}: synced at fingerprint ")))
        .with_context(|| format!("{name}: the second machine printed no fingerprint"))?
        .trim()
        .to_owned();
    ensure!(
        reported == checked,
        "{name}: the second machine checked fingerprint {reported}, but this tree's covered code is {checked}; nothing was synced"
    );
    ensure!(
        local_git(root, &["rev-parse", "HEAD"])? == head
            && fingerprint(root, verification)? == checked,
        "{name}: HEAD or the covered code changed while the check ran; nothing was synced"
    );
    let text = fs::read_to_string(root.join(MANIFEST))?;
    fs::write(root.join(MANIFEST), with_fingerprint(&text, name, &checked))?;
    println!("{name}: synced at fingerprint {checked}, checked on a second machine");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_remote_run_stops_when_the_second_machine_lacks_a_tool_the_gate_needs() {
        let work = tempfile::tempdir().unwrap();
        let bin = work.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let run = |path: &Path| {
            Command::new("/bin/sh")
                .arg("-c")
                .arg(format!("{}echo ready", remote_preflight()))
                .env("PATH", path)
                .env("HOME", work.path())
                .env_remove("STATECHART")
                .output()
                .unwrap()
        };
        let bare = run(&bin);
        assert_eq!(bare.status.code(), Some(REMOTE_TOOLS_MISSING));
        let message = String::from_utf8_lossy(&bare.stderr);
        assert!(message.contains("7zz unsquashfs statechart"), "{message}");
        let runner = work
            .path()
            .join(".claude/skills/statechart/scripts/statechart");
        for tool in [bin.join("7zz"), bin.join("unsquashfs"), runner] {
            fs::create_dir_all(tool.parent().unwrap()).unwrap();
            fs::write(&tool, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
        }
        let ready = run(&bin);
        assert!(
            ready.status.success(),
            "{}",
            String::from_utf8_lossy(&ready.stderr)
        );
    }
    use std::{collections::BTreeSet, path::PathBuf};

    #[cfg(unix)]
    #[test]
    fn a_streamed_run_keeps_every_stdout_line_and_its_exit_status() {
        let run = streamed(
            Command::new("/bin/sh")
                .arg("-c")
                .arg("echo first; echo progress >&2; printf 'synced at fingerprint x'; exit 3"),
        )
        .unwrap();
        assert_eq!(run.status.code(), Some(3));
        assert_eq!(
            String::from_utf8(run.stdout).unwrap(),
            "first\nsynced at fingerprint x\n"
        );
    }

    #[test]
    fn the_remote_script_quotes_every_value_and_works_inside_the_checkouts_target() {
        let script = remote_script(
            "/a b/it's",
            "remote-sync/x",
            "0123abcd",
            &["verify-sync", "zip-crypto"],
        );
        assert!(script.contains(r"cd '/a b/it'\''s'"));
        assert!(script.contains("mktemp -d \"$base/target/remote-sync.XXXXXX\""));
        assert!(script.contains("cargo xtask 'verify-sync' 'zip-crypto'"));
        assert!(script.contains("--detach \"$run/tree\" '0123abcd'"));
        let wrapped = shell_quoted(&script);
        assert!(wrapped.starts_with('\'') && wrapped.ends_with('\''));
    }

    #[test]
    fn a_section_whose_code_calls_the_field_check_must_list_its_shared_items() {
        let section = |items: &[(&str, &str)]| Verification {
            name: "x-fields".to_owned(),
            skill: String::new(),
            fingerprint: String::new(),
            slow: false,
            full_checks: Vec::new(),
            items: items
                .iter()
                .map(|(path, opening)| ((*path).to_owned(), (*opening).to_owned()))
                .collect(),
        };
        let source = |path: &str| -> Result<String> {
            Ok(match path {
                "src/x.rs" => "fn header(b: &[u8]) -> Field<u8> {\n    field_check(true, \"x\")?;\n    Ok(b[0])\n}\nfn plain() -> RawField<u8> {\n    my_field_check(1)\n}\n".to_owned(),
                _ => "pub(crate) fn field_check(ok: bool) {}\npub(crate) struct BadField(u8);\npub(crate) type Field<T> = T;\n".to_owned(),
            })
        };
        let missing =
            missing_shared_items(&section(&[("src/x.rs", "fn header(")]), source).unwrap();
        assert_eq!(missing.len(), 3, "{missing:?}");
        assert!(missing[0].starts_with("item = src/lib.rs | pub(crate) fn field_check("));
        let mut whole = vec![("src/x.rs", "fn header(")];
        whole.extend(SHARED_FIELD_CHECK);
        assert!(
            missing_shared_items(&section(&whole), source)
                .unwrap()
                .is_empty()
        );
        assert!(
            missing_shared_items(&section(&[("src/x.rs", "fn plain(")]), source)
                .unwrap()
                .is_empty(),
            "a longer name that ends in the shared one is not a call to it"
        );
    }

    #[test]
    fn every_source_needs_a_formal_check_or_a_reason_and_never_both() {
        let owned = |names: &[&str]| {
            names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>()
        };
        let sources = owned(&["src/a.rs", "src/b.rs", "src/c.rs"]);
        assert!(
            unclassified(
                &sources,
                &owned(&["src/a.rs"]),
                &owned(&["src/b.rs", "src/c.rs"])
            )
            .is_empty()
        );
        let missing = unclassified(&sources, &owned(&["src/a.rs"]), &owned(&["src/b.rs"]));
        assert_eq!(missing.len(), 1);
        assert!(missing[0].starts_with("src/c.rs has no formal check"));
        let both = unclassified(
            &sources,
            &owned(&["src/a.rs", "src/b.rs"]),
            &owned(&["src/b.rs", "src/c.rs"]),
        );
        assert_eq!(both.len(), 1);
        assert!(both[0].starts_with("src/b.rs is covered"));
        let gone = unclassified(
            &sources,
            &owned(&["src/a.rs"]),
            &owned(&["src/b.rs", "src/c.rs", "src/d.rs"]),
        );
        assert_eq!(
            gone,
            ["verification/uncovered.txt lists src/d.rs, which does not exist"]
        );
    }

    #[test]
    fn line_endings_do_not_change_a_fingerprint() {
        assert_eq!(fnv1a(1, "a\r\nb\r\n".bytes()), fnv1a(1, "a\nb\n".bytes()));
    }

    #[test]
    fn items_end_at_their_closing_brace_or_a_semicolon_outside_brackets() {
        let source = "const A: u8 = 1;\nfn f() {\n    let s = \"}{\";\n    let c = '}';\n    // }\n    if x { y }\n}\nfn g() {}\n";
        assert_eq!(
            item_spans_starting_with(source, "const A"),
            ["const A: u8 = 1;"]
        );
        assert_eq!(
            item_spans_starting_with(source, "fn f("),
            ["fn f() {\n    let s = \"}{\";\n    let c = '}';\n    // }\n    if x { y }\n}"]
        );
        assert!(item_spans_starting_with(source, "fn missing").is_empty());
        let bracketed =
            "fn t<const N: usize>(x: [u8; N]) -> u8 {\n    x[0]\n}\nconst M: [u8; 2] = [1; 2];\n";
        assert_eq!(
            item_spans_starting_with(bracketed, "fn t<"),
            ["fn t<const N: usize>(x: [u8; N]) -> u8 {\n    x[0]\n}"]
        );
        assert_eq!(
            item_spans_starting_with(bracketed, "const M"),
            ["const M: [u8; 2] = [1; 2];"]
        );
    }

    #[test]
    fn an_item_starts_at_the_kani_attributes_above_it() {
        let source = "fn a() {}\n\n    #[kani::proof]\n    #[kani::stub(\n        f,\n        g\n    )]\n    #[kani::unwind(8)]\n    fn p() {\n    }\n#[derive(Debug)]\nstruct S {}\n";
        assert_eq!(
            item_spans_starting_with(source, "    fn p("),
            [
                "    #[kani::proof]\n    #[kani::stub(\n        f,\n        g\n    )]\n    #[kani::unwind(8)]\n    fn p() {\n    }"
            ]
        );
        assert_eq!(
            item_spans_starting_with(source, "struct S"),
            ["struct S {}"]
        );
        assert_eq!(item_spans_starting_with(source, "fn a("), ["fn a() {}"]);
    }

    fn workspace() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask lives inside the workspace")
            .to_path_buf()
    }

    fn sections_made_stale_by(file: &str, old: &str, new: &str) -> BTreeSet<String> {
        let root = workspace();
        let original = read_source(&root, file).unwrap();
        assert_eq!(original.matches(old).count(), 1, "{old:?} must occur once");
        let edited = original.replacen(old, new, 1);
        let with = |text: &str| {
            let text = text.to_owned();
            let root = root.clone();
            let file = file.to_owned();
            move |path: &str| {
                if path == file {
                    Ok(text.clone())
                } else {
                    read_source(&root, path)
                }
            }
        };
        manifest(&root)
            .unwrap()
            .into_iter()
            .filter(|verification| verification.items.iter().any(|(path, _)| path == file))
            .filter(|verification| {
                fingerprint_of(verification, with(&original)).unwrap()
                    != fingerprint_of(verification, with(&edited)).unwrap()
            })
            .map(|verification| verification.name)
            .collect()
    }

    #[test]
    fn each_lzma_edit_makes_stale_only_the_groups_whose_proofs_reach_it() {
        let kani = [
            "lzma-bits",
            "lzma-trees",
            "lzma-lengths",
            "lzma-literal",
            "lzma-matched-literal",
            "lzma-distances",
        ];
        let cases: [(&str, &str, &[&str]); 14] = [
            (
                "None if range.complete() => return Err(data_error(\"truncated LZMA stream\")),",
                "None if range.complete() => return Err(data_error(\"cut LZMA stream\")),",
                &["lzma"],
            ),
            (
                "fn copy_in_chunks(&mut self, source: usize, run: usize) {",
                "fn copy_in_chunks(&mut self, source: usize, run: usize) {\n        let _ = run;",
                &["lzma"],
            ),
            (
                "let mut matched = usize::from(match_byte) << 1;",
                "let mut matched = usize::from(match_byte) << 2;",
                &["lzma", "lzma-matched-literal"],
            ),
            (
                "let zero = probabilities[symbol << 1];",
                "let zero = probabilities[symbol << 2];",
                &["lzma", "lzma-literal"],
            ),
            (
                "let byte = u32::from(byte);",
                "let byte = u32::from(byte) % 225;",
                &["lzma", "lzma-headers", "lzma-literal"],
            ),
            (
                "(index << 1 | bit as usize) as u32 - (1 << bits)",
                "(index << 1 | bit as usize) as u32 - (1 << (bits - 1))",
                &["lzma", "lzma-trees", "lzma-lengths"],
            ),
            (
                "16 + range.tree(&mut self.high) as usize",
                "17 + range.tree(&mut self.high) as usize",
                &["lzma", "lzma-lengths"],
            ),
            (
                "let base = distance - slot;",
                "let base = distance - slot + 1;",
                &["lzma", "lzma-distances"],
            ),
            (
                "self.range >>= 1;",
                "self.range >>= 2;",
                &["lzma", "lzma-bits", "lzma-distances"],
            ),
            (
                "self.taken - (self.end - self.start) as u64",
                "self.taken - (self.end - self.start) as u64 + 1",
                &["lzma", "lzma-input"],
            ),
            (
                "assert!(length < LENGTH_SYMBOLS);",
                "assert!(length <= LENGTH_SYMBOLS);",
                &["lzma-lengths"],
            ),
            (
                "(probability - (moved_away_from >> MOVE_BITS)) as u16",
                "(probability - (moved_away_from >> (MOVE_BITS + 1))) as u16",
                &["lzma", kani[0], kani[1], kani[2], kani[3], kani[4], kani[5]],
            ),
            (
                "const TOP: u32 = 1 << 24;",
                "const TOP: u32 = 1 << 23;",
                &["lzma", kani[0], kani[1], kani[2], kani[3], kani[4], kani[5]],
            ),
            (
                "const SYMBOL_INPUT_MAX: usize = 64;",
                "const SYMBOL_INPUT_MAX: usize = 65;",
                &["lzma", "lzma-input"],
            ),
        ];
        for (old, new, expected) in cases {
            let expected: BTreeSet<String> =
                expected.iter().map(|name| (*name).to_owned()).collect();
            assert_eq!(
                sections_made_stale_by("src/lzma.rs", old, new),
                expected,
                "editing {old:?}"
            );
        }
    }

    #[test]
    fn each_parser_edit_makes_stale_only_the_section_whose_proofs_reach_it() {
        let cases = [
            (
                "src/native_ar.rs",
                "decimal(&bytes[48..58])",
                "decimal(&bytes[47..58])",
                "ar-fields",
            ),
            (
                "src/native_tar.rs",
                "\"tar numeric field has bytes after its terminator\",",
                "\"tar numeric field has bytes past its terminator\",",
                "tar-numbers",
            ),
            (
                "src/native_tar.rs",
                "field_check(value >= i64::MIN >> 8, \"tar base-256 field overflows\")?;",
                "field_check(value >= i64::MIN >> 7, \"tar base-256 field overflows\")?;",
                "tar-numbers",
            ),
            (
                "src/native_zip.rs",
                "field_check(!seen, \"ZIP entry has two ZIP64 extra fields\")?;",
                "field_check(seen, \"ZIP entry has two ZIP64 extra fields\")?;",
                "zip64-extra-field",
            ),
            (
                "src/native_7z.rs",
                "#[kani::unwind(8)]\n    fn ppmd_properties_read_only_five_bytes",
                "#[kani::unwind(2)]\n    fn ppmd_properties_read_only_five_bytes",
                "7z-ppmd-properties",
            ),
        ];
        for (file, old, new, section) in cases {
            assert_eq!(
                sections_made_stale_by(file, old, new),
                BTreeSet::from([section.to_owned()]),
                "editing {old:?} in {file}"
            );
        }
    }

    #[test]
    fn every_manifest_item_is_fingerprinted_to_its_end() {
        let root = workspace();
        let mut cut = Vec::new();
        for verification in manifest(&root).unwrap() {
            for (path, opening) in &verification.items {
                let source = read_source(&root, path).unwrap();
                for text in item_spans_starting_with(&source, opening) {
                    let code = crate::kani::blank_strings_and_comments(text);
                    let open = code.matches(['[', '(']).count();
                    let close = code.matches([']', ')']).count();
                    if open != close {
                        cut.push(format!("{}: {path} | {opening}", verification.name));
                    }
                }
            }
        }
        assert!(cut.is_empty(), "items cut short:\n{}", cut.join("\n"));
    }
    #[test]
    fn a_sync_writes_the_fingerprint_of_a_section_that_has_none_yet() {
        let text = "[first]\nskill = a.md\nfingerprint = old\n\n[fresh]\nskill = b.md\nfull-check = true\n";
        let synced = with_fingerprint(text, "fresh", "new");
        assert_eq!(
            synced,
            "[first]\nskill = a.md\nfingerprint = old\n\n[fresh]\nfingerprint = new\nskill = b.md\nfull-check = true\n"
        );
        let resynced = with_fingerprint(&synced, "fresh", "newer");
        assert_eq!(resynced.matches("fingerprint = ").count(), 2);
        assert!(resynced.contains("[fresh]\nfingerprint = newer\n"));
        assert!(resynced.contains("[first]\nskill = a.md\nfingerprint = old\n"));
    }
}
