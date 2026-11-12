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
mod kani;
mod minimums;
mod private_paths;
mod pure_rust;
mod sevenzip_lookups;
mod verify;

use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const USAGE: &str = "usage: cargo xtask gate fast|full [--remote] | lint-docs | lint-comments | lint-7zip-lookups | lint-private-paths | lint-pure-rust | check-minimums | install-hooks | ci-wait | warm-cache | format-tests | consumer-crate | lint-commit-msg FILE | lint-commits | verify [--owed] | verify-sync NAME [--remote] | kani-reach NAME | bench [scoreboard.py run options]";

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
        ["gate", kind @ ("fast" | "full"), "--remote"] => verify::gate_remotely(&root(), kind),
        ["lint-docs"] => docs::lint(&root()),
        ["lint-comments"] => comments::lint(&root()),
        ["lint-7zip-lookups"] => sevenzip_lookups::lint(&root()),
        ["lint-private-paths"] => private_paths::lint(&root()),
        ["lint-pure-rust"] => pure_rust::lint(&root()),
        ["check-minimums"] => minimums::check(&root()),
        ["install-hooks"] => install_hooks(),
        ["ci-wait"] => ci_wait(),
        ["ship"] => ship(),
        ["warm-cache"] => warm_cache(),
        ["release-checks"] => release_checks(),
        ["format-tests"] => format_tests(),
        ["consumer-crate"] => consumer_crate(),
        ["lint-commit-msg", file] => commits::lint_message_file(&root(), Path::new(file)),
        ["lint-commits"] => commits::lint_unpushed(&root()),
        ["verify-sync", name] => verify::record_checked_fingerprint(&root(), name),
        ["verify-sync", name, "--remote"] => {
            verify::record_remotely_checked_fingerprint(&root(), name)
        }
        ["verify"] => verify::fail_on_stale_fingerprints(&root()),
        ["verify", "--owed"] => verify::fail_on_owed_slow_checks(&root()),
        ["kani-reach", name] => kani::fail_on_unlisted_kani_reach(&root(), name),
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

fn target_dir() -> PathBuf {
    root().join(env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()))
}

fn cargo(args: &[&str]) -> Result<()> {
    cargo_with(args, &[])
}

const GIT_LOCATION_VARIABLES: [&str; 3] = ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"];

fn cargo_command(args: &[&str], envs: &[(&str, &str)]) -> Command {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command.current_dir(root());
    for name in GIT_LOCATION_VARIABLES {
        command.env_remove(name);
    }
    command.envs(envs.iter().copied()).args(args);
    command
}

fn cargo_with(args: &[&str], envs: &[(&str, &str)]) -> Result<()> {
    let status = cargo_command(args, envs).status()?;
    ensure!(
        status.success(),
        "cargo {} exited with {status}",
        args.join(" ")
    );
    Ok(())
}

const CI_APPEAR_ATTEMPTS: u32 = 24;
const CI_APPEAR_INTERVAL: Duration = Duration::from_secs(5);
const NETWORK_ATTEMPTS: u32 = 4;
const NETWORK_BACKOFF: Duration = Duration::from_secs(15);
const TRANSIENT_NETWORK_ERRORS: &[&str] = &[
    "HTTP 5",
    "Could not resolve host",
    "Recv failure",
    "unable to access",
    "Connection reset",
    "Connection refused",
    "timed out",
    "Can't assign requested address",
    "error connecting to api.github.com",
];

fn is_transient_network_error(message: &str) -> bool {
    TRANSIENT_NETWORK_ERRORS
        .iter()
        .any(|error| message.contains(error))
}

fn with_network_retries<T>(what: &str, mut attempt: impl FnMut() -> Result<T>) -> Result<T> {
    let mut tried = 1;
    loop {
        match attempt() {
            Err(error)
                if tried < NETWORK_ATTEMPTS
                    && is_transient_network_error(&format!("{error:#}")) =>
            {
                eprintln!(
                    "xtask: {what} hit a network error, retrying ({tried} of {NETWORK_ATTEMPTS}): {error:#}"
                );
                std::thread::sleep(NETWORK_BACKOFF * tried);
                tried += 1;
            }
            outcome => return outcome,
        }
    }
}

fn github(args: &[&str]) -> Result<String> {
    with_network_retries("gh", || output("gh", args))
}

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
    let run = ci_run(&head, None)?;
    watch_ci_run(&run, &head)
}

fn ci_run(head: &str, created_since: Option<&str>) -> Result<String> {
    let pushed = output("git", &["branch", "-r", "--contains", head])?;
    ensure!(
        !pushed.is_empty(),
        "HEAD {head} is not on any remote branch; push it first"
    );
    for _ in 0..CI_APPEAR_ATTEMPTS {
        let runs = github(&[
            "run",
            "list",
            "--commit",
            head,
            "--json",
            "databaseId,createdAt",
        ])?;
        if let Some(run) = newest_run(&runs, created_since)? {
            return Ok(run);
        }
        std::thread::sleep(CI_APPEAR_INTERVAL);
    }
    bail!("no CI run appeared for {head}")
}

fn newest_run(runs: &str, created_since: Option<&str>) -> Result<Option<String>> {
    let runs: serde_json::Value = serde_json::from_str(runs)?;
    let mut candidates: Vec<(&str, String)> = runs
        .as_array()
        .context("gh run list did not return a list")?
        .iter()
        .filter_map(|run| Some((run["createdAt"].as_str()?, run["databaseId"].to_string())))
        .filter(|(created, _)| created_since.is_none_or(|since| *created >= since))
        .collect();
    candidates.sort();
    Ok(candidates.pop().map(|(_, id)| id))
}

fn watch_ci_run(run: &str, head: &str) -> Result<()> {
    for _ in 0..NETWORK_ATTEMPTS {
        let _ = Command::new("gh")
            .current_dir(root())
            .args(["run", "watch", run, "--interval", "15"])
            .stdout(std::process::Stdio::null())
            .status()?;
        let state = github(&[
            "run",
            "view",
            run,
            "--json",
            "status,conclusion",
            "--jq",
            r#""\(.status) \(.conclusion)""#,
        ])?;
        if !state.starts_with("completed") {
            eprintln!("xtask: watching CI run {run} stopped while it was {state}; watching again");
            continue;
        }
        println!(
            "{}",
            github(&[
                "run",
                "view",
                run,
                "--json",
                "conclusion,jobs",
                "--jq",
                r#""run \(.conclusion)", (.jobs[] | "\(.name) \(.conclusion)")"#
            ])?
        );
        ensure!(
            state == "completed success",
            "CI run {run} for {head} failed"
        );
        return Ok(());
    }
    bail!("CI run {run} for {head} did not complete while watched {NETWORK_ATTEMPTS} times")
}

fn release_checks() -> Result<()> {
    verify::fail_on_owed_slow_checks(&root())?;
    verify::run_every_full_check(&root())?;
    kani::fail_on_kani_list_mismatch(&root())?;
    format_tests()?;
    gate(true)
}

fn ship() -> Result<()> {
    ensure!(
        output("git", &["status", "--porcelain"])?.is_empty(),
        "commit or stash every change before shipping"
    );
    commits::lint_unpushed(&root())?;
    let head = output("git", &["rev-parse", "HEAD"])?;
    let branch = format!("ci/{}", &head[..12]);
    let push = |args: &[&str]| -> Result<()> {
        with_network_retries("git push", || output("git", args).map(drop))
    };
    let leftover = with_network_retries("git ls-remote", || {
        output(
            "git",
            &["ls-remote", "origin", &format!("refs/heads/{branch}")],
        )
    })?;
    if !leftover.is_empty() {
        push(&[
            "push",
            "--quiet",
            "--no-verify",
            "origin",
            "--delete",
            &branch,
        ])?;
    }
    let pushed_at = output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"])?;
    push(&[
        "push",
        "--no-verify",
        "origin",
        &format!("HEAD:refs/heads/{branch}"),
    ])?;
    let run = ci_run(&head, Some(&pushed_at))?;
    let checked = watch_ci_run(&run, &head).or_else(|failure| {
        let host = match env::consts::OS {
            "macos" => "macos",
            "linux" => "ubuntu",
            other => other,
        };
        let jobs = github(
            &["run", "view", &run, "--json", "jobs", "--jq",
              r#".jobs[] | "\(.name)\t\(.conclusion)\t\(.steps | length)""#],
        )?;
        let unstarted_host_job_only = jobs.lines().all(|job| {
            let fields: Vec<&str> = job.split('\t').collect();
            fields.get(1) == Some(&"success")
                || (fields.get(1) == Some(&"cancelled")
                    && fields.get(2) == Some(&"0")
                    && fields[0].contains(host))
        });
        ensure!(unstarted_host_job_only, failure);
        println!("xtask: the {host} CI job never got a runner; running the full gate on this {host} host instead");
        gate(true)
    });
    push(&["push", "--quiet", "origin", "--delete", &branch])?;
    checked?;
    push(&[
        "push",
        "--no-verify",
        "origin",
        &format!("{head}:refs/heads/main"),
    ])?;
    println!("shipped {head} to main after its CI run passed");
    Ok(())
}

const ZLIB_CRATES: &[&str] = &[
    "flate2",
    "libz-sys",
    "libz-rs-sys",
    "zlib-rs",
    "miniz_oxide",
];

fn ensure_no_zlib() -> Result<()> {
    let tree = output(
        "cargo",
        &[
            "tree",
            "--package",
            "zipper",
            "--edges",
            "normal",
            "--target",
            "all",
            "--locked",
            "--prefix",
            "none",
        ],
    )?;
    let mut zlibs = zlib_crates_in(&tree);
    if cfg!(target_os = "macos") {
        let binary = target_dir().join("release/zipper");
        let linked = output("otool", &["-L", &binary.to_string_lossy()])?;
        let native = pure_rust::non_system_libraries(&linked);
        ensure!(
            native.is_empty(),
            "the release binary links {native:?}; Zipper builds from Rust only"
        );
        zlibs.extend(zlib_libraries_in(&linked));
    }
    ensure!(
        zlibs.is_empty(),
        "the release build depends on {zlibs:?}; Zipper decodes Deflate with src/inflate.rs"
    );
    Ok(())
}

fn zlib_crates_in(tree: &str) -> Vec<String> {
    tree.lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| ZLIB_CRATES.contains(name))
        .map(str::to_owned)
        .collect()
}

fn zlib_libraries_in(otool: &str) -> Vec<String> {
    otool
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|library| library.contains("libz."))
        .map(str::to_owned)
        .collect()
}

type Step = fn() -> Result<()>;

const CLIPPY: &[&str] = &[
    "clippy",
    "--workspace",
    "--all-targets",
    "--locked",
    "--",
    "-D",
    "warnings",
];
const CI_TESTS: &[&str] = &[
    "nextest",
    "run",
    "--workspace",
    "--locked",
    "--cargo-profile",
    "ci",
];
const EXTRA_FEATURE_SUBSETS: &[&str] = &["zip,7z", ""];
const FEATURES_WITHOUT_LIBRARY_CODE: &[&str] = &["cli"];
const FORMAT_TESTS: &[(&str, &[&str])] = &[
    ("zip", &["zip_corpus"]),
    ("zip-deflate", &["zip_method_features", "zstd_feature"]),
    ("zip-encryption", &["zip_method_features"]),
    ("zip-legacy", &["zip_method_features"]),
    ("zip-deflate64", &["zip_method_features"]),
    ("zip-bzip2", &["zip_method_features"]),
    ("zip-lzma", &["zip_method_features"]),
    ("zip-ppmd", &["zip_method_features"]),
    ("7z", &["sevenz_corpus"]),
    ("tar,zstd", &["tar_corpus"]),
    ("cpio", &["hostile_cpio"]),
    ("ar,zstd", &["hostile_ar"]),
    ("rpm,zstd", &["hostile_rpm"]),
    ("xar", &["xar_corpus"]),
    ("cab", &["cab_corpus", "cab_sets"]),
    ("lzh", &["lzh_corpus"]),
    ("arj", &["arj_corpus"]),
    ("iso", &["iso_corpus"]),
    ("wim", &["wim_corpus"]),
    ("exe", &["exe_corpus"]),
    ("swf,flv", &["swf_corpus"]),
    ("lp", &["lp_corpus", "hostile_lp"]),
    ("uefi", &["uefi_corpus", "hostile_uefi"]),
    ("ihex", &["ihex_corpus", "hostile_ihex"]),
    (
        "cramfs,ext,squashfs,zstd",
        &["squashfs_corpus", "ext_corpus"],
    ),
    ("tar,squashfs", &["zstd_feature"]),
];
const CONSUMER_ARCHIVES: &[&str] = &[
    "tests/data/zip-corpus/7zip-deflate.zip",
    "tests/data/zip-corpus/manifest.tsv",
    "tests/data/7z-corpus/7zip-bzip2-long.7z",
    "tests/data/7z-corpus/manifest.tsv",
];
const DEFLATE_CONSUMER_ARCHIVES: &[&str] = &[
    "tests/data/zip-corpus/7zip-deflate.zip",
    "tests/data/zip-corpus/manifest.tsv",
    "tests/data/zip-corpus/python-stored.zip",
    "tests/data/zip-corpus/manifest.tsv",
];
const OPTIONAL_ZIP_CRATES: &[&str] = &[
    "aes",
    "ctr",
    "deflate64",
    "hmac",
    "pbkdf2",
    "ppmd-rust",
    "sha1",
];
const DEFLATE_SUBSET_TESTS: &[&str] = &[
    "nextest",
    "run",
    "--package",
    "zipper",
    "--locked",
    "--cargo-profile",
    "ci",
    "--no-default-features",
    "--features",
    "zip-deflate",
    "--test",
    "zip_method_features",
    "--test",
    "zstd_feature",
];
const DEFLATE_UNIT_TESTS_BUILD: &[&str] = &[
    "test",
    "--package",
    "zipper",
    "--locked",
    "--profile",
    "ci",
    "--no-default-features",
    "--features",
    "zip-deflate",
    "--lib",
    "--no-run",
];
const SUBSET_TESTS: &[&str] = &[
    "nextest",
    "run",
    "--package",
    "zipper",
    "--locked",
    "--cargo-profile",
    "ci",
    "--no-default-features",
    "--features",
    "zip,7z",
    "--test",
    "feature_subset",
    "--test",
    "zip_corpus",
    "--test",
    "sevenz_corpus",
    "--test",
    "zstd_feature",
    "--test",
    "zip_method_features",
];
const RELEASE_BUILD: &[&str] = &["build", "--release", "--locked", "--package", "zipper"];
const COUNTERS_CLIPPY: &[&str] = &[
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
];
const COUNTERS_TEST: &[&str] = &[
    "test",
    "--package",
    "zipper",
    "--features",
    "counters",
    "--locked",
    "--test",
    "counters",
];

fn cargo_features() -> Result<Vec<String>> {
    let manifest = fs::read_to_string(root().join("Cargo.toml"))?;
    let table = manifest
        .split_once("\n[features]\n")
        .context("Cargo.toml has no [features] table")?
        .1;
    Ok(table
        .lines()
        .take_while(|line| !line.starts_with('['))
        .filter_map(|line| line.split_once(" = "))
        .map(|(name, _)| name.trim().to_owned())
        .filter(|name| name != "default")
        .collect())
}

fn feature_subsets() -> Result<Vec<String>> {
    let mut subsets = cargo_features()?;
    subsets.extend(
        EXTRA_FEATURE_SUBSETS
            .iter()
            .map(|subset| subset.to_string()),
    );
    Ok(subsets)
}

fn lint_feature_subsets() -> Result<()> {
    for features in feature_subsets()? {
        let mut args = vec![
            "clippy",
            "--package",
            "zipper",
            "--lib",
            "--locked",
            "--no-default-features",
            "--features",
            &features,
            "--",
            "-D",
            "warnings",
        ];
        if !FEATURES_WITHOUT_LIBRARY_CODE.contains(&features.as_str()) {
            args.extend(["-D", "unused_crate_dependencies"]);
        }
        cargo(&args)?;
    }
    Ok(())
}

fn consumer_crate() -> Result<()> {
    run_consumer(&[], CONSUMER_ARCHIVES)?;
    run_consumer(&["--no-default-features"], DEFLATE_CONSUMER_ARCHIVES)?;
    ensure_deflate_build_leaves_out_optional_zip_crates()
}

fn run_consumer(features: &[&str], archives: &[&str]) -> Result<()> {
    let work = tempfile::tempdir()?;
    let destination = work
        .path()
        .to_str()
        .context("temporary path is not UTF-8")?;
    cargo(
        &[
            &["run", "--locked", "--quiet", "--package", "zipper-consumer"],
            features,
            &["--", destination],
            archives,
        ]
        .concat(),
    )
}

fn ensure_deflate_build_leaves_out_optional_zip_crates() -> Result<()> {
    let tree = output(
        "cargo",
        &[
            "tree",
            "--package",
            "zipper",
            "--edges",
            "normal",
            "--target",
            "all",
            "--locked",
            "--prefix",
            "none",
            "--no-default-features",
            "--features",
            "zip-deflate",
        ],
    )?;
    let optional = crates_named_in(&tree, OPTIONAL_ZIP_CRATES);
    ensure!(
        optional.is_empty(),
        "a zip-deflate build depends on {optional:?}, which only optional ZIP methods and encryption need"
    );
    Ok(())
}

fn crates_named_in(tree: &str, names: &[&str]) -> Vec<String> {
    let mut found: Vec<String> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| names.contains(name))
        .map(str::to_owned)
        .collect();
    found.sort();
    found.dedup();
    found
}

fn format_tests() -> Result<()> {
    ensure_nextest()?;
    let formats = cargo_features()?;
    for (features, tests) in FORMAT_TESTS {
        ensure!(
            features
                .split(',')
                .all(|feature| formats.iter().any(|name| name == feature)),
            "FORMAT_TESTS names a feature Cargo.toml lacks in {features}"
        );
        let mut args = vec![
            "nextest",
            "run",
            "--package",
            "zipper",
            "--locked",
            "--cargo-profile",
            "ci",
            "--no-default-features",
            "--features",
            features,
        ];
        for test in *tests {
            args.extend(["--test", test]);
        }
        cargo_with(&args, &required_7zip_comparisons_outside_ci())?;
    }
    Ok(())
}

fn warm_cache() -> Result<()> {
    cargo(CLIPPY)?;
    lint_feature_subsets()?;
    windows_clippy()?;
    ensure_nextest()?;
    cargo(&[CI_TESTS, &["--no-run"]].concat())?;
    cargo(&[SUBSET_TESTS, &["--no-run"]].concat())?;
    cargo(&[DEFLATE_SUBSET_TESTS, &["--no-run"]].concat())?;
    cargo(DEFLATE_UNIT_TESTS_BUILD)?;
    cargo(&["build", "--locked", "--package", "zipper-consumer"])?;
    cargo(&[
        "build",
        "--locked",
        "--package",
        "zipper-consumer",
        "--no-default-features",
    ])?;
    cargo(RELEASE_BUILD)?;
    cargo(COUNTERS_CLIPPY)?;
    cargo(&[COUNTERS_TEST, &["--no-run"]].concat())
}

fn gate(full: bool) -> Result<()> {
    let mut steps: Vec<(&str, Step)> = vec![
        ("fmt", || cargo(&["fmt", "--all", "--check"])),
        ("docs", || docs::lint(&root())),
        ("comments", || comments::lint(&root())),
        ("7-zip lookups", || sevenzip_lookups::lint(&root())),
        ("private paths", || private_paths::lint(&root())),
        ("pure rust", || pure_rust::lint(&root())),
        ("results", || scoreboard(&["check"])),
        ("hooks", hook_tests),
        ("verify", || verify::fail_on_stale_fingerprints(&root())),
        ("parity", || python_script("scripts/parity.py", &["check"])),
        ("clippy", || cargo(CLIPPY)),
        ("test", || {
            ensure_nextest()?;
            cargo_with(CI_TESTS, &required_7zip_comparisons_outside_ci())
        }),
        ("feature subsets", || {
            lint_feature_subsets()?;
            ensure_nextest()?;
            cargo(SUBSET_TESTS)?;
            cargo(DEFLATE_SUBSET_TESTS)?;
            cargo(DEFLATE_UNIT_TESTS_BUILD)
        }),
        ("consumer crate", consumer_crate),
        ("windows clippy", windows_clippy),
        ("release build", || {
            cargo(RELEASE_BUILD)?;
            ensure_no_zlib()
        }),
        ("release round trip", round_trip),
    ];
    if full {
        steps.push(("counters", || {
            cargo(COUNTERS_CLIPPY)?;
            cargo(COUNTERS_TEST)
        }));
        steps.push(("benchmark smoke", benchmark_smoke));
        steps.push(("dependency minimums", || minimums::check(&root())));
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

const REQUIRE_7Z: &str = "ZIPPER_PARITY_REQUIRE_7Z";

fn required_7zip_comparisons_outside_ci() -> Vec<(&'static str, &'static str)> {
    if env::var_os("GITHUB_ACTIONS").is_some() || env::var_os(REQUIRE_7Z).is_some() {
        Vec::new()
    } else {
        vec![(REQUIRE_7Z, "1")]
    }
}

const NEXTEST_INSTALL: &str = "cargo install cargo-nextest --locked";

fn ensure_nextest() -> Result<()> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let installed = Command::new(cargo)
        .args(["nextest", "--version"])
        .output()
        .is_ok_and(|output| output.status.success());
    ensure!(
        installed,
        "the gate runs its tests with cargo-nextest; install it with `{NEXTEST_INSTALL}`"
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
        ])
        .arg(target_dir().join("windows-clippy"))
        .args(["--", "-D", "warnings"])
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
    for setting in [
        ["core.hooksPath", ".githooks"],
        [
            "merge.coverage.name",
            "one section of verification/coverage.txt at a time",
        ],
        [
            "merge.coverage.driver",
            "python3 scripts/merge_coverage.py %O %A %B",
        ],
    ] {
        let status = Command::new("git")
            .current_dir(root())
            .arg("config")
            .args(setting)
            .status()?;
        ensure!(status.success(), "git config exited with {status}");
    }
    println!(
        "commit-msg now checks cited hashes, pre-push runs `cargo xtask lint-commits` and `cargo xtask gate fast`, and verification/coverage.txt merges one section at a time"
    );
    Ok(())
}

fn round_trip() -> Result<()> {
    let binary = target_dir()
        .join("release")
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

    write_zip(&tree, &dir.join("a.zip"))?;
    let listing = succeed(&["list", "a.zip"])?;
    ensure!(listing.contains("tree/many/f299.txt"), "list omits a file");
    for threads in ["1", "4"] {
        let selected = format!("selected-t{threads}");
        succeed(&[
            "--threads",
            threads,
            "extract",
            "a.zip",
            "-o",
            &selected,
            "--path",
            "tree/many",
        ])?;
        same_tree(&tree.join("many"), &dir.join(&selected).join("tree/many"))?;
        ensure!(
            top_level(&dir.join(&selected).join("tree"))? == 1,
            "selected extraction wrote files outside tree/many"
        );
    }
    succeed(&["test", "a.zip"])?;
    for threads in ["1", "4"] {
        let out = format!("native-zip-t{threads}");
        succeed(&["--threads", threads, "extract", "a.zip", "-o", &out])?;
        same_tree(&tree, &dir.join(&out).join("tree"))?;
    }

    let bytes = fs::read(dir.join("a.zip"))?;
    fs::write(dir.join("truncated.zip"), &bytes[..bytes.len() / 2])?;
    let output = run(&["extract", "truncated.zip", "-o", "truncated-out"])?;
    ensure!(!output.status.success(), "truncated a.zip extracted");
    ensure!(
        !dir.join("truncated-out").exists(),
        "truncated a.zip left output behind"
    );
    Ok(())
}

fn benchmark_smoke() -> Result<()> {
    let work = tempfile::tempdir()?;
    let tree = work.path().join("tree");
    write_fixture(&tree)?;
    // EXTERNAL(bsdtar): macOS bsdtar rewrites UTF-8 names to NFD.
    fs::remove_file(tree.join("\u{fc}nicode-\u{65e5}.txt"))?;
    let report = work.path().join("matrix.json");
    let binary = target_dir()
        .join("release")
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
    for needle in ["\"native-1\"", "\"median_cpu_seconds\"", "\"parity\""] {
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
    for script in [
        ".claude/hooks/test_guard_destructive.py",
        "scripts/test_plant.py",
        "scripts/test_merge_coverage.py",
    ] {
        let status = Command::new(python()?)
            .current_dir(root())
            .arg(script)
            .status()?;
        ensure!(status.success(), "{script} exited with {status}");
    }
    Ok(())
}

fn python_script(script: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(python()?)
        .current_dir(root())
        .arg(script)
        .args(args)
        .status()?;
    ensure!(
        status.success(),
        "{script} {} exited with {status}",
        args.join(" ")
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
    cargo(RELEASE_BUILD)?;
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

#[cfg(test)]
mod zlib_check_tests {
    use super::*;

    #[test]
    fn every_zlib_crate_in_a_tree_and_every_linked_libz_is_named() {
        let clean = "zipper v0.1.0 (/z)\ncrc32fast v1.5.3\nlibbz2-rs-sys v0.2.6\nzstd-sys v2.1.2\n";
        assert!(zlib_crates_in(clean).is_empty());
        for name in [
            "flate2",
            "libz-sys",
            "libz-rs-sys",
            "zlib-rs",
            "miniz_oxide",
        ] {
            let tree = format!("{clean}{name} v1.0.0\n");
            assert_eq!(zlib_crates_in(&tree), [name.to_string()], "{name}");
        }
        let linked = "target/release/zipper:\n\t/usr/lib/libz.1.dylib (compatibility version 1.0.0)\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0)\n";
        assert_eq!(zlib_libraries_in(linked), ["/usr/lib/libz.1.dylib"]);
        let unlinked =
            "target/release/zipper:\n\t/usr/lib/libiconv.2.dylib (compatibility version 7.0.0)\n";
        assert!(zlib_libraries_in(unlinked).is_empty());
    }
}

#[cfg(test)]
mod cargo_command_tests {
    use super::*;

    #[test]
    fn gate_steps_never_pass_a_hooks_git_location_to_the_tests() {
        let command = cargo_command(&["test"], &[("ZIPPER_X", "1")]);
        let envs: Vec<_> = command.get_envs().collect();
        for name in GIT_LOCATION_VARIABLES {
            assert!(
                envs.contains(&(std::ffi::OsStr::new(name), None)),
                "{name} reaches the tests"
            );
        }
        assert!(envs.contains(&(
            std::ffi::OsStr::new("ZIPPER_X"),
            Some(std::ffi::OsStr::new("1"))
        )));
    }
}

#[cfg(test)]
mod ci_run_tests {
    use super::newest_run;

    const RUNS: &str = r#"[
        {"databaseId": 43117428460, "createdAt": "2026-11-11T23:08:03Z"},
        {"databaseId": 43098311931, "createdAt": "2026-11-11T20:49:44Z"}
    ]"#;

    #[test]
    fn a_run_created_before_the_push_is_never_watched() {
        assert_eq!(
            newest_run(RUNS, Some("2026-11-11T23:08:00Z"))
                .unwrap()
                .as_deref(),
            Some("43117428460")
        );
        assert_eq!(
            newest_run(RUNS, Some("2026-11-11T23:09:00Z")).unwrap(),
            None
        );
        let stale_first = r#"[{"databaseId": 1, "createdAt": "2026-11-11T20:49:44Z"},
                             {"databaseId": 2, "createdAt": "2026-11-11T23:08:03Z"}]"#;
        assert_eq!(newest_run(stale_first, None).unwrap().as_deref(), Some("2"));
    }
}

#[cfg(test)]
mod network_retry_tests {
    use super::{is_transient_network_error, with_network_retries};
    use std::cell::Cell;

    #[test]
    fn the_network_errors_seen_on_11_november_are_retried_and_a_failed_run_is_not() {
        for seen in [
            "gh run view exited with exit status: 1: failed to get jobs: HTTP 502: Server Error (https://api.github.com/repos/o/r/actions/runs/1/jobs?per_page=100)",
            "git push exited with exit status: 128: fatal: unable to access 'https://github.com/o/r/': Could not resolve host: github.com",
            "git push exited with exit status: 128: fatal: unable to access 'https://github.com/o/r/': Recv failure: Can't assign requested address",
            "error connecting to api.github.com",
        ] {
            assert!(is_transient_network_error(seen), "{seen}");
        }
        for failure in [
            "CI run 42 for abc failed",
            "git push exited with exit status: 1: ! [rejected] HEAD -> main (non-fast-forward)",
            "gh run list exited with exit status: 1: HTTP 404: Not Found",
        ] {
            assert!(!is_transient_network_error(failure), "{failure}");
        }
    }

    #[test]
    fn a_rejected_push_is_tried_once() {
        let tries = Cell::new(0);
        let outcome: anyhow::Result<()> = with_network_retries("push", || {
            tries.set(tries.get() + 1);
            anyhow::bail!("! [rejected] non-fast-forward")
        });
        assert!(outcome.is_err());
        assert_eq!(tries.get(), 1);
    }
}
