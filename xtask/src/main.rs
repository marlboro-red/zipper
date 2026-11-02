use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::{Duration, Instant},
};
mod comments;
mod commits;
mod docs;

use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const USAGE: &str = "usage: cargo xtask gate fast|full | lint-docs | lint-comments | install-hooks | ci-wait | lint-commit-msg FILE | lint-commits | bench [scoreboard.py run options]";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if env::var_os(STUB_C_TOOLCHAIN_ENV).is_some() {
        return match stub_c_toolchain(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("xtask stub C toolchain: {error:#}");
                ExitCode::FAILURE
            }
        };
    }
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["gate", "fast"] => gate(false),
        ["gate", "full"] => gate(true),
        ["lint-docs"] => docs::lint(&root()),
        ["lint-comments"] => comments::lint(&root()),
        ["install-hooks"] => install_hooks(),
        ["ci-wait"] => ci_wait(),
        ["lint-commit-msg", file] => commits::lint_message_file(&root(), Path::new(file)),
        ["lint-commits"] => commits::lint_unpushed(&root()),
        ["bench", ref rest @ ..] => bench(rest),
        _ => Err(anyhow::anyhow!(USAGE)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives inside the workspace")
        .to_path_buf()
}

fn cargo(args: &[&str]) -> Result<()> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(root())
        .args(args)
        .status()?;
    ensure!(
        status.success(),
        "cargo {} exited with {status}",
        args.join(" ")
    );
    Ok(())
}

const CI_APPEAR_ATTEMPTS: u32 = 24;
const CI_APPEAR_INTERVAL: Duration = Duration::from_secs(5);

fn output(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .current_dir(root())
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "{program} {} exited with {}: {}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn ci_wait() -> Result<()> {
    let head = output("git", &["rev-parse", "HEAD"])?;
    let pushed = output("git", &["branch", "-r", "--contains", &head])?;
    ensure!(
        !pushed.is_empty(),
        "HEAD {head} is not on any remote branch; push it first"
    );
    let mut run = String::new();
    for _ in 0..CI_APPEAR_ATTEMPTS {
        run = output(
            "gh",
            &[
                "run",
                "list",
                "--commit",
                &head,
                "--json",
                "databaseId",
                "--jq",
                ".[0].databaseId // empty",
            ],
        )?;
        if !run.is_empty() {
            break;
        }
        std::thread::sleep(CI_APPEAR_INTERVAL);
    }
    ensure!(!run.is_empty(), "no CI run appeared for {head}");
    let watched = Command::new("gh")
        .current_dir(root())
        .args(["run", "watch", &run, "--exit-status", "--interval", "15"])
        .stdout(std::process::Stdio::null())
        .status()?;
    println!(
        "{}",
        output(
            "gh",
            &[
                "run",
                "view",
                &run,
                "--json",
                "conclusion,jobs",
                "--jq",
                r#""run \(.conclusion)", (.jobs[] | "\(.name) \(.conclusion)")"#
            ]
        )?
    );
    ensure!(watched.success(), "CI run {run} for {head} failed");
    Ok(())
}

const MACOS_SYSTEM_ZLIB: &str = "/usr/lib/libz.1.dylib";

// RECORD(benchmarks/native-zip/system-zlib/README.md): Apple's zlib is the measured decoder.
fn ensure_system_zlib() -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    let binary = root().join("target/release/zipper");
    let libraries = output("otool", &["-L", &binary.to_string_lossy()])?;
    let zlibs: Vec<&str> = libraries
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|library| library.contains("libz."))
        .collect();
    ensure!(
        zlibs == [MACOS_SYSTEM_ZLIB],
        "release binary links {zlibs:?}, not {MACOS_SYSTEM_ZLIB}; unset PKG_CONFIG_PATH entries that point at another zlib"
    );
    Ok(())
}

type Step = fn() -> Result<()>;

fn gate(full: bool) -> Result<()> {
    let mut steps: Vec<(&str, Step)> = vec![
        ("fmt", || cargo(&["fmt", "--all", "--check"])),
        ("docs", || docs::lint(&root())),
        ("comments", || comments::lint(&root())),
        ("results", || scoreboard(&["check"])),
        ("hooks", hook_tests),
        ("clippy", || {
            cargo(&[
                "clippy",
                "--workspace",
                "--all-targets",
                "--locked",
                "--",
                "-D",
                "warnings",
            ])
        }),
        ("test", || cargo(&["test", "--workspace", "--locked"])),
        ("windows clippy", windows_clippy),
        ("release build", || {
            cargo(&["build", "--release", "--locked", "--package", "zipper"])?;
            ensure_system_zlib()
        }),
        ("release round trip", round_trip),
    ];
    if full {
        steps.push(("release test", || {
            cargo(&["test", "--workspace", "--locked", "--release"])
        }));
        steps.push(("counters", || {
            cargo(&[
                "clippy",
                "--package",
                "zipper",
                "--all-targets",
                "--features",
                "counters",
                "--locked",
                "--",
                "-D",
                "warnings",
            ])?;
            cargo(&[
                "test",
                "--package",
                "zipper",
                "--features",
                "counters",
                "--locked",
                "--test",
                "counters",
            ])
        }));
        steps.push(("benchmark smoke", benchmark_smoke));
    }
    let mut rows = Vec::new();
    for (name, step) in &steps {
        eprintln!("xtask: gate step {name}");
        let start = Instant::now();
        let outcome = step();
        if let Err(error) = &outcome {
            eprintln!("xtask: {name} failed: {error:#}");
        }
        rows.push((*name, outcome.is_ok(), start.elapsed().as_secs_f64()));
    }
    println!("\n| step | result | seconds |\n| --- | --- | ---: |");
    for (name, passed, seconds) in &rows {
        let result = if *passed { "pass" } else { "FAIL" };
        println!("| {name} | {result} | {seconds:.1} |");
    }
    let failed = rows.iter().filter(|row| !row.1).count();
    ensure!(
        failed == 0,
        "gate {} failed {failed} step(s)",
        if full { "full" } else { "fast" }
    );
    Ok(())
}

const STUB_C_TOOLCHAIN_ENV: &str = "XTASK_STUB_C_TOOLCHAIN";
const WINDOWS_TARGET: &str = "x86_64-pc-windows-gnu";

fn windows_clippy() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let sysroot = Command::new(rustc).args(["--print", "sysroot"]).output()?;
    let sysroot = PathBuf::from(String::from_utf8(sysroot.stdout)?.trim());
    ensure!(
        sysroot.join("lib/rustlib").join(WINDOWS_TARGET).is_dir(),
        "run `rustup target add {WINDOWS_TARGET}` for this toolchain"
    );
    let tool = env::current_exe()?;
    let suffix = WINDOWS_TARGET.replace('-', "_");
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(root())
        .env(STUB_C_TOOLCHAIN_ENV, "1")
        .env(format!("CC_{suffix}"), &tool)
        .env(format!("AR_{suffix}"), &tool)
        .args([
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--target",
            WINDOWS_TARGET,
            "--target-dir",
            "target/windows-clippy",
            "--",
            "-D",
            "warnings",
        ])
        .status()?;
    ensure!(status.success(), "Windows clippy exited with {status}");
    Ok(())
}

fn stub_c_toolchain(args: &[String]) -> Result<()> {
    if let Some(at) = args.iter().position(|arg| arg == "-o") {
        let output = args.get(at + 1).context("-o without a file")?;
        fs::write(output, b"")?;
    }
    for archive in args.iter().filter(|arg| arg.ends_with(".a")) {
        fs::write(archive, b"!<arch>\n")?;
    }
    Ok(())
}

fn install_hooks() -> Result<()> {
    let status = Command::new("git")
        .current_dir(root())
        .args(["config", "core.hooksPath", ".githooks"])
        .status()?;
    ensure!(status.success(), "git config exited with {status}");
    println!(
        "commit-msg now checks cited hashes, and pre-push runs `cargo xtask lint-commits` and `cargo xtask gate fast`"
    );
    Ok(())
}

fn round_trip() -> Result<()> {
    let binary = root()
        .join("target/release")
        .join(format!("zipper{}", env::consts::EXE_SUFFIX));
    ensure!(
        binary.is_file(),
        "missing release binary {}",
        binary.display()
    );
    let work = tempfile::tempdir()?;
    let dir = work.path();
    let tree = dir.join("tree");
    write_fixture(&tree)?;
    let run = |args: &[&str]| {
        Command::new(&binary)
            .current_dir(dir)
            .args(args)
            .env("ZIPPER_7Z", dir.join("no-backend"))
            .output()
            .with_context(|| format!("launch zipper {}", args.join(" ")))
    };
    let succeed = |args: &[&str]| -> Result<String> {
        let output = run(args)?;
        ensure!(
            output.status.success(),
            "zipper {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    };

    for threads in ["1", "4"] {
        let archive = format!("t{threads}.zipper");
        let out = format!("native-t{threads}");
        let selected = format!("selected-t{threads}");
        succeed(&[
            "--threads",
            threads,
            "create",
            &archive,
            "tree",
            "--level",
            "3",
        ])?;
        succeed(&["test", &archive])?;
        let listing = succeed(&["list", &archive])?;
        ensure!(listing.contains("tree/many/f299.txt"), "list omits a file");
        succeed(&["--threads", threads, "extract", &archive, "-o", &out])?;
        same_tree(&tree, &dir.join(&out).join("tree"))?;
        ensure!(top_level(&dir.join(&out))? == 1, "extra output beside tree");
        succeed(&["extract", &archive, "-o", &selected, "--path", "tree/many"])?;
        same_tree(&tree.join("many"), &dir.join(&selected).join("tree/many"))?;
        ensure!(
            top_level(&dir.join(&selected).join("tree"))? == 1,
            "selected extraction wrote files outside tree/many"
        );
    }

    write_zip(&tree, &dir.join("a.zip"))?;
    for engine in ["native", "native-fast"] {
        succeed(&["--zip-engine", engine, "test", "a.zip"])?;
        for threads in ["1", "4"] {
            let out = format!("{engine}-zip-t{threads}");
            succeed(&[
                "--zip-engine",
                engine,
                "--threads",
                threads,
                "extract",
                "a.zip",
                "-o",
                &out,
            ])?;
            same_tree(&tree, &dir.join(&out).join("tree"))?;
        }
    }

    for (archive, engine) in [("t1.zipper", "native"), ("a.zip", "native")] {
        let bytes = fs::read(dir.join(archive))?;
        let truncated = format!("truncated-{archive}");
        fs::write(dir.join(&truncated), &bytes[..bytes.len() / 2])?;
        let out = format!("truncated-out-{archive}");
        let output = run(&["--zip-engine", engine, "extract", &truncated, "-o", &out])?;
        ensure!(!output.status.success(), "truncated {archive} extracted");
        ensure!(
            !dir.join(&out).exists(),
            "truncated {archive} left output behind"
        );
    }
    Ok(())
}

fn benchmark_smoke() -> Result<()> {
    let work = tempfile::tempdir()?;
    let tree = work.path().join("tree");
    write_fixture(&tree)?;
    // EXTERNAL(bsdtar): macOS bsdtar rewrites UTF-8 names to NFD.
    fs::remove_file(tree.join("\u{fc}nicode-\u{65e5}.txt"))?;
    let report = work.path().join("matrix.json");
    let binary = root()
        .join("target/release")
        .join(format!("zipper{}", env::consts::EXE_SUFFIX));
    let mut args = vec![
        root().join("scripts/extraction_matrix.py").into_os_string(),
        tree.into_os_string(),
        "--zipper".into(),
        binary.into_os_string(),
        "--output".into(),
        report.clone().into_os_string(),
        "--work-dir".into(),
        work.path().into(),
        "--threads".into(),
        "1".into(),
        "2".into(),
        "--runs".into(),
        "1".into(),
    ];
    if Command::new("bsdtar").arg("--version").output().is_ok() {
        args.extend(["--tar".into(), "bsdtar".into()]);
    }
    let status = Command::new(python()?)
        .current_dir(root())
        .args(&args)
        .status()?;
    ensure!(
        status.success(),
        "extraction_matrix.py exited with {status}"
    );
    let report = fs::read_to_string(report)?;
    for needle in [
        "\"native-1\"",
        "\"native-fast-1\"",
        "\"median_cpu_seconds\"",
        "\"parity\"",
    ] {
        ensure!(report.contains(needle), "benchmark report lacks {needle}");
    }
    Ok(())
}

fn python() -> Result<&'static str> {
    ["python3", "python"]
        .into_iter()
        .find(|name| Command::new(name).arg("--version").output().is_ok())
        .context("python3 or python is required")
}

fn hook_tests() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }
    let status = Command::new(python()?)
        .current_dir(root())
        .arg(".claude/hooks/test_guard_destructive.py")
        .status()?;
    ensure!(
        status.success(),
        "test_guard_destructive.py exited with {status}"
    );
    Ok(())
}

fn scoreboard(args: &[&str]) -> Result<()> {
    let status = Command::new(python()?)
        .current_dir(root())
        .arg("scripts/scoreboard.py")
        .args(args)
        .status()?;
    ensure!(
        status.success(),
        "scoreboard.py {} exited with {status}",
        args.join(" ")
    );
    Ok(())
}

fn bench(args: &[&str]) -> Result<()> {
    cargo(&["build", "--release", "--locked", "--package", "zipper"])?;
    cargo(&[
        "build",
        "--release",
        "--locked",
        "--package",
        "zipper",
        "--features",
        "counters",
        "--example",
        "count_zip",
        "--target-dir",
        "target/counters",
    ])?;
    scoreboard(&[&["run"], args].concat())
}

fn write_fixture(tree: &Path) -> Result<()> {
    fs::create_dir_all(tree.join("a/b/c"))?;
    fs::create_dir_all(tree.join("empty-dir"))?;
    fs::create_dir_all(tree.join("many"))?;
    fs::write(tree.join("a/b/c/deep.txt"), b"deep\n")?;
    fs::write(tree.join("empty.bin"), b"")?;
    fs::write(tree.join("\u{fc}nicode-\u{65e5}.txt"), "\u{fc}\n")?;
    for index in 0..300 {
        let body = format!("file {index}\n").repeat(index % 17);
        fs::write(tree.join(format!("many/f{index}.txt")), body)?;
    }
    let mut log = Vec::new();
    for line in 0..60_000 {
        writeln!(log, "{line:08} INFO request served in {} ms", line % 97)?;
    }
    fs::write(tree.join("text.log"), log)?;
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let noise: Vec<u8> = (0..(1 << 20) + 1)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    fs::write(tree.join("noise.bin"), noise)?;
    Ok(())
}

fn write_zip(tree: &Path, path: &Path) -> Result<()> {
    let mut writer = ZipWriter::new(fs::File::create(path)?);
    let base = tree.parent().context("fixture has a parent")?;
    for (index, (relative, contents)) in entries(base)?.into_iter().enumerate() {
        let method = if index % 2 == 0 {
            CompressionMethod::Deflated
        } else {
            CompressionMethod::Stored
        };
        let options = SimpleFileOptions::default().compression_method(method);
        let name = relative
            .to_str()
            .context("fixture path is UTF-8")?
            .replace('\\', "/");
        match contents {
            None => writer.add_directory(name, options)?,
            Some(bytes) => {
                writer.start_file(name, options)?;
                writer.write_all(&bytes)?;
            }
        }
    }
    writer.finish()?;
    Ok(())
}

fn top_level(dir: &Path) -> Result<usize> {
    Ok(fs::read_dir(dir)?.count())
}

type Tree = BTreeMap<PathBuf, Option<Vec<u8>>>;

fn entries(base: &Path) -> Result<Tree> {
    let mut tree = Tree::new();
    for entry in walkdir::WalkDir::new(base).min_depth(1).sort_by_file_name() {
        let entry = entry?;
        let relative = entry.path().strip_prefix(base)?.to_path_buf();
        let kind = entry.file_type();
        let contents = if kind.is_dir() {
            None
        } else if kind.is_file() {
            Some(fs::read(entry.path())?)
        } else {
            bail!("unexpected file type at {}", entry.path().display());
        };
        tree.insert(relative, contents);
    }
    Ok(tree)
}

fn same_tree(expected: &Path, actual: &Path) -> Result<()> {
    let (expected_tree, actual_tree) = (entries(expected)?, entries(actual)?);
    for (path, contents) in &expected_tree {
        match actual_tree.get(path) {
            None => bail!("{} is missing {}", actual.display(), path.display()),
            Some(found) if found != contents => {
                bail!("{} differs at {}", actual.display(), path.display())
            }
            Some(_) => {}
        }
    }
    if let Some(extra) = actual_tree
        .keys()
        .find(|path| !expected_tree.contains_key(*path))
    {
        bail!("{} has unexpected {}", actual.display(), extra.display());
    }
    Ok(())
}
