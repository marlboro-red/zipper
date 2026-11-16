use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const BENCH: &str = env!("CARGO_BIN_EXE_zipper-decode-bench");
const SAMPLE_BYTES: usize = 150_000;

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn python() -> &'static str {
    ["python3", "python"]
        .into_iter()
        .find(|name| Command::new(name).arg("--version").output().is_ok())
        .expect("python3 or python is required")
}

fn run(command: &mut Command) -> Vec<u8> {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn sample() -> Vec<u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut out = Vec::with_capacity(SAMPLE_BYTES);
    while out.len() < SAMPLE_BYTES {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        match state % 4 {
            0 => out.extend(state.to_le_bytes()),
            1 => out.extend(std::iter::repeat_n(b'z', (state >> 8) as usize % 300)),
            _ => out.extend(format!("line {} of the decode bench sample\n", state % 1000).bytes()),
        }
    }
    out.truncate(SAMPLE_BYTES);
    out
}

#[test]
fn one_round_on_a_small_input_records_every_decoder_with_its_bytes_checked() {
    let work = tempfile::tempdir().unwrap();
    let original = work.path().join("sample.tar");
    fs::write(&original, sample()).unwrap();
    let streams = work.path().join("streams");
    run(Command::new(BENCH)
        .arg("corpus")
        .arg(&original)
        .arg(&streams));
    let report = work.path().join("decode.json");
    run(Command::new(python())
        .arg(repository().join("scripts/m31_decoders.py"))
        .args(["decode", "--harness", BENCH, "--corpus"])
        .arg(&streams)
        .arg("--original")
        .arg(&original)
        .args(["--rounds", "1", "--trial", "--allow-busy", "--out"])
        .arg(&report));
    let listed: Value = serde_json::from_slice(&run(Command::new(BENCH).arg("decoders"))).unwrap();
    let json: Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();

    assert_eq!(json["schema"], "m31-decoders/1");
    assert_eq!(json["trial"], true);
    for key in ["platform", "machine", "cpu_count"] {
        assert!(!json["machine"][key].is_null(), "machine lacks {key}");
    }
    assert!(json["revision"]["commit"].is_string() || json["revision"]["commit"].is_null());
    assert_eq!(json["corpus"]["original"]["bytes"], SAMPLE_BYTES);
    assert_eq!(
        json["corpus"]["original"]["sha256"].as_str().unwrap().len(),
        64
    );
    let decode = &json["decode"];
    assert_eq!(decode["rounds"], 1);
    assert_eq!(decode["warmup_rounds"], 1);
    assert_eq!(
        decode["order_including_warmup"].as_array().unwrap().len(),
        2
    );
    let rows = listed.as_array().unwrap();
    assert!(rows.len() >= 20, "only {} decoders listed", rows.len());
    for row in rows {
        let (codec, decoder) = (
            row["codec"].as_str().unwrap(),
            row["decoder"].as_str().unwrap(),
        );
        let stream = &json["corpus"]["streams"][codec];
        assert_eq!(stream["sha256"].as_str().unwrap().len(), 64, "{codec}");
        let result = &decode["results"][codec][decoder];
        let samples = result["samples"]
            .as_array()
            .unwrap_or_else(|| panic!("{codec} {decoder}"));
        assert_eq!(samples.len(), 1, "{codec} {decoder}");
        let sample = &samples[0];
        assert_eq!(sample["verified_bytes"], SAMPLE_BYTES, "{codec} {decoder}");
        for key in [
            "seconds",
            "process_seconds",
            "process_cpu_seconds",
            "peak_rss_bytes",
        ] {
            assert!(
                sample[key].as_f64().is_some(),
                "{codec} {decoder} lacks {key}"
            );
        }
        for key in [
            "median_seconds",
            "min_seconds",
            "max_seconds",
            "peak_rss_bytes",
        ] {
            assert!(
                result[key].as_f64().is_some(),
                "{codec} {decoder} lacks {key}"
            );
        }
        for name in row["crates"].as_array().unwrap() {
            let name = name.as_str().unwrap();
            assert!(
                result["crates"][name].is_string(),
                "{codec} {decoder} lacks the version of {name}"
            );
        }
    }
}

#[test]
fn a_decoded_byte_that_differs_from_the_original_fails_the_sample() {
    let work = tempfile::tempdir().unwrap();
    let original = work.path().join("sample.tar");
    let bytes = sample();
    fs::write(&original, &bytes).unwrap();
    let streams = work.path().join("streams");
    run(Command::new(BENCH)
        .arg("corpus")
        .arg(&original)
        .arg(&streams));
    let mut changed = bytes;
    changed[SAMPLE_BYTES / 2] ^= 1;
    let other = work.path().join("other.tar");
    fs::write(&other, changed).unwrap();
    for (codec, decoder, extension) in [
        ("gzip", "zipper", "gz"),
        ("xpress", "xpress-huffman", "xpress"),
    ] {
        let output = Command::new(BENCH)
            .args(["decode", codec, decoder])
            .arg(streams.join(format!("sample.tar.{extension}")))
            .arg(&other)
            .arg(work.path().join("report.json"))
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{codec} {decoder} passed");
        assert!(stderr.contains("differ from the original"), "{stderr}");
    }
    let refused = Command::new(python())
        .arg(repository().join("scripts/m31_decoders.py"))
        .args(["decode", "--harness", BENCH, "--corpus"])
        .arg(&streams)
        .arg("--original")
        .arg(&other)
        .args(["--rounds", "1", "--trial", "--allow-busy"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success());
    assert!(stderr.contains("is not the original"), "{stderr}");
}

fn driver() -> Command {
    let mut command = Command::new(python());
    command.arg(repository().join("scripts/m31_decoders.py"));
    command
}

fn small_corpus(work: &Path) -> PathBuf {
    let source = work.join("source").join("silesia");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("sample"), sample()).unwrap();
    let out = work.join("corpus");
    run(driver()
        .args(["corpus", "--harness", BENCH, "--out"])
        .arg(&out)
        .arg("--silesia")
        .arg(&source));
    out
}

#[test]
fn a_failed_extraction_row_is_recorded_and_the_run_keeps_its_other_rows() {
    let work = tempfile::tempdir().unwrap();
    let corpus = small_corpus(work.path());
    let report = work.path().join("all.json");
    let failed = driver()
        .args(["all", "--harness", BENCH, "--zipper", BENCH, "--corpus"])
        .arg(&corpus)
        .args(["--only", "gzip", "zstd", "--rounds", "1", "--runs", "1"])
        .args(["--trial", "--allow-busy", "--out"])
        .arg(&report)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(!failed.status.success(), "a run with failed rows passed");
    assert!(stderr.contains("gzip, zstd"), "{stderr}");
    let json: Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
    for codec in ["gzip", "zstd"] {
        let decoders = json["decode"]["results"][codec].as_object().unwrap();
        assert!(!decoders.is_empty(), "{codec}");
        for (decoder, result) in decoders {
            assert_eq!(
                result["samples"].as_array().unwrap().len(),
                1,
                "{codec} {decoder}"
            );
        }
        let row = &json["extract"]["results"][codec];
        assert!(row["error"].is_string(), "{codec}: {row}");
        assert!(
            row["stderr"].as_str().unwrap().contains("Round 0"),
            "{codec}: {row}"
        );
    }
    assert_eq!(
        json["extract"]["failed"],
        serde_json::json!(["gzip", "zstd"])
    );
    assert!(
        json["extract"]["skipped"]["lzma"]
            .as_str()
            .unwrap()
            .contains("silesia.tar")
    );
}

#[test]
fn corpus_moves_the_fetch_manifest_out_of_the_tree_the_wim_captures() {
    let work = tempfile::tempdir().unwrap();
    let out = work.path().join("corpus");
    let tree = out.join("tree");
    fs::create_dir_all(tree.join("silesia")).unwrap();
    fs::write(tree.join("silesia").join("sample"), sample()).unwrap();
    fs::write(tree.join("silesia.manifest.json"), "{}\n").unwrap();
    let corpus = || {
        driver()
            .args(["corpus", "--harness", BENCH, "--out"])
            .arg(&out)
            .output()
            .unwrap()
    };
    let moved = corpus();
    assert!(
        moved.status.success(),
        "{}",
        String::from_utf8_lossy(&moved.stderr)
    );
    let names: Vec<_> = fs::read_dir(&tree)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["silesia"]);
    assert_eq!(
        fs::read_to_string(out.join("silesia.manifest.json")).unwrap(),
        "{}\n"
    );
    fs::write(tree.join("stray"), "").unwrap();
    let refused = corpus();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "a tree with a stray file passed");
    assert!(stderr.contains("stray"), "{stderr}");
}

const CRATE_WRITER: &str = r#"
import hashlib, io, sys, tarfile
crate, entries = sys.argv[1], sys.argv[2:]
with tarfile.open(crate, "w:gz") as archive:
    for entry in entries:
        name, _, text = entry.partition("=")
        data = text.encode()
        info = tarfile.TarInfo(name)
        info.size = len(data)
        archive.addfile(info, io.BytesIO(data))
with open(crate, "rb") as written:
    print(hashlib.sha256(written.read()).hexdigest())
"#;

struct FakeRegistry {
    cargo_home: PathBuf,
    lockfile: PathBuf,
    packages: String,
}

impl FakeRegistry {
    fn new(work: &Path) -> Self {
        FakeRegistry {
            cargo_home: work.join("cargo-home"),
            lockfile: work.join("Cargo.lock"),
            packages: "version = 4\n\n[[package]]\nname = \"local\"\nversion = \"0.1.0\"\n"
                .to_owned(),
        }
    }

    fn crate_file(&self, index: &str, name: &str, version: &str, entries: &[&str]) -> String {
        let folder = self.cargo_home.join("registry/cache").join(index);
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(format!("{name}-{version}.crate"));
        let digest = run(Command::new(python())
            .args(["-I", "-c", CRATE_WRITER])
            .arg(&path)
            .args(entries));
        String::from_utf8(digest).unwrap().trim().to_owned()
    }

    fn lock(&mut self, name: &str, version: &str, checksum: &str) {
        self.packages.push_str(&format!(
            "\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n\
             source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
             checksum = \"{checksum}\"\n"
        ));
    }

    fn corpus(&self, out: &Path) -> std::process::Output {
        fs::write(&self.lockfile, &self.packages).unwrap();
        driver()
            .args(["corpus", "--harness", BENCH, "--source", "dependencies"])
            .arg("--out")
            .arg(out)
            .arg("--lockfile")
            .arg(&self.lockfile)
            .env("CARGO_HOME", &self.cargo_home)
            .output()
            .unwrap()
    }
}

#[test]
fn the_dependency_corpus_holds_each_locked_crate_whose_checksum_matches() {
    let work = tempfile::tempdir().unwrap();
    let mut registry = FakeRegistry::new(work.path());
    let alpha = registry.crate_file(
        "index.crates.io-test",
        "alpha",
        "1.0.0",
        &[
            "alpha-1.0.0/src/lib.rs=pub fn alpha() {}\n",
            "alpha-1.0.0/README=alpha\n",
        ],
    );
    registry.crate_file(
        "index.crates.io-decoy",
        "beta",
        "0.2.0",
        &["beta-0.2.0/src/lib.rs=a copy the lockfile does not pin\n"],
    );
    let beta = registry.crate_file(
        "index.crates.io-test",
        "beta",
        "0.2.0",
        &["beta-0.2.0/src/lib.rs=pub fn beta() {}\n"],
    );
    registry.lock("alpha", "1.0.0", &alpha);
    registry.lock("beta", "0.2.0", &beta);
    let out = work.path().join("corpus");
    let built = registry.corpus(&out);
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );

    let tree = out.join("tree");
    let names: Vec<_> = fs::read_dir(&tree)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["dependencies"]);
    let source = tree.join("dependencies");
    assert_eq!(
        fs::read_to_string(source.join("alpha-1.0.0/src/lib.rs")).unwrap(),
        "pub fn alpha() {}\n"
    );
    assert_eq!(
        fs::read_to_string(source.join("beta-0.2.0/src/lib.rs")).unwrap(),
        "pub fn beta() {}\n"
    );
    assert!(!source.join("local-0.1.0").exists());
    assert!(out.join("streams/dependencies.tar.gz").is_file());
    let record: Value =
        serde_json::from_slice(&fs::read(out.join("corpus-source.json")).unwrap()).unwrap();
    assert_eq!(record["name"], "dependencies");
    assert_eq!(record["tar"], "dependencies.tar");
    assert_eq!(record["files"], 3);
    assert_eq!(record["tar_sha256"].as_str().unwrap().len(), 64);
    let crates = record["provenance"]["crates"].as_array().unwrap();
    assert_eq!(crates.len(), 2);
    assert_eq!(crates[1]["sha256"], beta.as_str());

    let first = fs::read(out.join("dependencies.tar")).unwrap();
    let again = registry.corpus(&out);
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert_eq!(fs::read(out.join("dependencies.tar")).unwrap(), first);

    let report = work.path().join("extract.json");
    let extracted = driver()
        .args(["extract", "--zipper", BENCH, "--corpus"])
        .arg(&out)
        .args(["--only", "gzip", "--runs", "1", "--trial", "--allow-busy"])
        .arg("--out")
        .arg(&report)
        .output()
        .unwrap();
    assert!(!extracted.status.success(), "the bench extracted a tar");
    let json: Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
    assert_eq!(json["source"]["name"], "dependencies");
    assert!(json["extract"]["results"]["gzip"]["error"].is_string());
    assert!(
        json["extract"]["skipped"]["lzma"]
            .as_str()
            .unwrap()
            .contains("dependencies.tar")
    );
}

#[test]
fn the_dependency_corpus_refuses_a_crate_it_cannot_reproduce_on_every_machine() {
    let cases: [(&[&str], &str); 3] = [
        (
            &["gamma-1.0.0/README=a", "gamma-1.0.0/readme=b"],
            "case-insensitive",
        ),
        (&["gamma-1.0.0/../escape=a"], "outside gamma-1.0.0/"),
        (&["other-1.0.0/src/lib.rs=a"], "outside gamma-1.0.0/"),
    ];
    for (entries, refusal) in cases {
        let work = tempfile::tempdir().unwrap();
        let mut registry = FakeRegistry::new(work.path());
        let gamma = registry.crate_file("index.crates.io-test", "gamma", "1.0.0", entries);
        registry.lock("gamma", "1.0.0", &gamma);
        let out = work.path().join("corpus");
        let refused = registry.corpus(&out);
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(!refused.status.success(), "{entries:?} passed");
        assert!(stderr.contains(refusal), "{entries:?}: {stderr}");
        assert!(!out.join("tree/dependencies.partial").exists());
        assert!(!out.join("tree/dependencies").exists());
    }

    let work = tempfile::tempdir().unwrap();
    let mut registry = FakeRegistry::new(work.path());
    registry.crate_file(
        "index.crates.io-test",
        "gamma",
        "1.0.0",
        &["gamma-1.0.0/src/lib.rs=a"],
    );
    registry.lock("gamma", "1.0.0", &"0".repeat(64));
    let refused = registry.corpus(&work.path().join("corpus"));
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success(),
        "a crate with another checksum passed"
    );
    assert!(stderr.contains("cargo fetch --locked"), "{stderr}");

    let mixed = driver()
        .args(["corpus", "--harness", BENCH, "--source", "dependencies"])
        .arg("--out")
        .arg(work.path().join("mixed"))
        .arg("--silesia")
        .arg(work.path())
        .output()
        .unwrap();
    assert!(!mixed.status.success());
    assert!(String::from_utf8_lossy(&mixed.stderr).contains("--silesia needs --source silesia"));
}

fn repeat(codec: &str, decoder: &str, stream: &Path, original: &Path, count: &str) -> Output {
    Command::new(BENCH)
        .args(["repeat", codec, decoder])
        .arg(stream)
        .arg(original)
        .arg(count)
        .output()
        .unwrap()
}

#[test]
fn repeat_decodes_the_stream_count_times_and_checks_the_last_pass() {
    let work = tempfile::tempdir().unwrap();
    let original = work.path().join("sample.tar");
    let bytes = sample();
    fs::write(&original, &bytes).unwrap();
    let streams = work.path().join("streams");
    run(Command::new(BENCH)
        .arg("corpus")
        .arg(&original)
        .arg(&streams));
    for (codec, extension, c_library) in [
        ("zstd", "zst", "libzstd"),
        ("xz", "xz", "liblzma"),
        ("lzma", "lzma", "liblzma"),
    ] {
        let stream = streams.join(format!("sample.tar.{extension}"));
        for decoder in ["zipper", c_library] {
            for count in [0u64, 3] {
                let output = repeat(codec, decoder, &stream, &original, &count.to_string());
                assert!(
                    output.status.success(),
                    "{codec} {decoder} {count}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let row: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(row["codec"], codec);
                assert_eq!(row["decoder"], decoder);
                assert_eq!(row["repeats"], count);
                assert_eq!(row["verified_bytes"], count.min(1) * SAMPLE_BYTES as u64);
            }
        }
    }
    let mut changed = bytes;
    changed[SAMPLE_BYTES - 1] ^= 1;
    let other = work.path().join("other.tar");
    fs::write(&other, changed).unwrap();
    let zst = streams.join("sample.tar.zst");
    let output = repeat("zstd", "zipper", &zst, &other, "2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "a changed original passed");
    assert!(stderr.contains("differ from the original"), "{stderr}");
    let output = repeat("zstd", "zipper", &zst, &original, "many");
    assert!(
        !output.status.success(),
        "a count that is not a number passed"
    );
}

#[cfg(unix)]
const STAND_IN_PERF: &str = r##"#!/usr/bin/env python3
import subprocess, sys
args = sys.argv[1:]
def command():
    return args[args.index("--") + 1:]
def path_after(flag):
    return args[args.index(flag) + 1]
if args[0] == "stat":
    subprocess.run(command(), check=True)
    repeats = int(command()[-1])
    with open(path_after("-o"), "w") as out:
        out.write("# started on a stand-in\n\n")
        for event in path_after("-e").split(","):
            value = "<not supported>" if event == "L1-dcache-load-misses" else str(1000 + 100 * repeats)
            out.write(f"{value},,cpu_core/{event}/,1000,100.00,,\n")
elif args[0] == "record":
    subprocess.run(command(), check=True)
    open(path_after("-o"), "w").write("samples\n")
elif args[0] == "report" and "-q" in args:
    print("    61.00%  [.] zipper::zstandard::Sequences::execute")
    print("    20.00%  [k] clear_page_erms")
    print("    12.50%  [.] __memmove_avx_unaligned_erms")
elif args[0] == "report":
    print("# the whole report")
elif args[0] == "annotate":
    print("annotated " + command()[0])
else:
    sys.exit("unexpected " + " ".join(args))
"##;

#[cfg(unix)]
#[test]
fn the_m35_profile_counts_one_decode_and_annotates_zippers_top_symbols() {
    use std::os::unix::fs::PermissionsExt;
    let work = tempfile::tempdir().unwrap();
    let corpus = small_corpus(work.path());
    let perf = work.path().join("perf");
    fs::write(&perf, STAND_IN_PERF).unwrap();
    fs::set_permissions(&perf, fs::Permissions::from_mode(0o755)).unwrap();
    let out = work.path().join("profile");
    run(Command::new(python())
        .arg(repository().join("scripts/m35_profile.py"))
        .args([
            "--harness",
            BENCH,
            "--codec",
            "all",
            "--repeats",
            "3",
            "--corpus",
        ])
        .arg(&corpus)
        .arg("--perf")
        .arg(&perf)
        .arg("--out")
        .arg(&out));
    let profile: Value =
        serde_json::from_slice(&fs::read(out.join("profile.json")).unwrap()).unwrap();
    assert_eq!(profile["repeats"], 3);
    let source: Value =
        serde_json::from_slice(&fs::read(corpus.join("corpus-source.json")).unwrap()).unwrap();
    for (codec, c_library) in [("zstd", "libzstd"), ("xz", "liblzma"), ("lzma", "liblzma")] {
        for decoder in ["zipper", c_library] {
            let row = &profile["results"][codec][decoder];
            assert_eq!(row["per_decode"]["cycles"], 100.0, "{codec} {decoder}");
            assert!(row["per_decode"]["L1-dcache-load-misses"].is_null());
            assert!(out.join(format!("report-{codec}-{decoder}.txt")).exists());
            let pass: Value = serde_json::from_slice(
                &fs::read(out.join(format!("repeat-{codec}-{decoder}-decode.json"))).unwrap(),
            )
            .unwrap();
            assert_eq!(pass["repeats"], 3);
            assert_eq!(pass["verified_bytes"], source["tar_bytes"]);
        }
        let annotated = profile["results"][codec]["zipper"]["annotated"]
            .as_array()
            .unwrap();
        assert_eq!(
            annotated.len(),
            2,
            "{codec}: only user symbols are annotated"
        );
        assert_eq!(
            annotated[0]["symbol"],
            "zipper::zstandard::Sequences::execute"
        );
        let first = fs::read_to_string(out.join(format!("annotate-{codec}-zipper-1.txt"))).unwrap();
        assert_eq!(
            first.trim(),
            "annotated zipper::zstandard::Sequences::execute"
        );
        assert!(
            profile["results"][codec][c_library]["annotated"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    let summary = fs::read_to_string(out.join("summary.md")).unwrap();
    for heading in ["## zstd", "## xz", "## lzma"] {
        assert!(summary.contains(heading), "{summary}");
    }
    assert!(summary.contains("not counted"), "{summary}");
}

fn ab(corpus: &Path, base: &Path, codec: &str, rounds: &str, out: &Path) -> Output {
    Command::new(python())
        .arg(repository().join("scripts/m35_ab.py"))
        .args(["--harness", BENCH, "--base-harness"])
        .arg(base)
        .args(["--base-commit", "feedface", "--base-ref", "main"])
        .args(["--codec", codec, "--rounds", rounds, "--corpus"])
        .arg(corpus)
        .args(["--trial", "--allow-busy", "--out"])
        .arg(out)
        .output()
        .unwrap()
}

#[test]
fn the_m35_ab_times_both_zippers_and_the_c_library_in_one_order_with_every_byte_checked() {
    let work = tempfile::tempdir().unwrap();
    let corpus = small_corpus(work.path());
    let source: Value =
        serde_json::from_slice(&fs::read(corpus.join("corpus-source.json")).unwrap()).unwrap();
    let out = work.path().join("ab");
    let timed = ab(&corpus, Path::new(BENCH), "all", "2", &out);
    assert!(
        timed.status.success(),
        "{}",
        String::from_utf8_lossy(&timed.stderr)
    );
    let json: Value = serde_json::from_slice(&fs::read(out.join("ab.json")).unwrap()).unwrap();
    assert_eq!(json["schema"], "m31-decoders/1");
    assert_eq!(json["ab"]["base_commit"], "feedface");
    assert_eq!(json["ab"]["base_ref"], "main");
    assert_eq!(json["ab"]["head_commit"], json["revision"]["commit"]);
    assert!(json["machine"].get("cpu").is_some());
    assert_eq!(
        json["corpus"]["base_harness"]["sha256"],
        json["corpus"]["harness"]["sha256"]
    );
    let decode = &json["decode"];
    let order = decode["order_including_warmup"].as_array().unwrap();
    assert_eq!(order.len(), 3);
    assert_eq!(order[0].as_array().unwrap().len(), 9);
    assert!(
        order[0]
            .as_array()
            .unwrap()
            .contains(&Value::from("xz zipper@base"))
    );
    let results = decode["results"].as_object().unwrap();
    assert_eq!(results.len(), 3);
    for (codec, c_library) in [("zstd", "libzstd"), ("xz", "liblzma"), ("lzma", "liblzma")] {
        let rows = results[codec].as_object().unwrap();
        let mut names: Vec<_> = rows.keys().map(String::as_str).collect();
        names.sort_unstable();
        let mut expected = vec!["zipper", "zipper@base", c_library];
        expected.sort_unstable();
        assert_eq!(names, expected, "{codec}");
        for (name, row) in rows {
            let samples = row["samples"].as_array().unwrap();
            assert_eq!(samples.len(), 2, "{codec} {name}");
            for sample in samples {
                assert_eq!(sample["verified_bytes"], source["tar_bytes"]);
            }
        }
        assert_eq!(rows["zipper@base"]["harness"], "base");
    }
    let table = fs::read_to_string(out.join("ab.md")).unwrap();
    for needle in [
        "| zstd | head over base |",
        "| zstd | head over libzstd |",
        "| lzma | head over liblzma |",
        "feedface",
        "trial",
    ] {
        assert!(table.contains(needle), "{needle}: {table}");
    }

    let one = work.path().join("one");
    let timed = ab(&corpus, Path::new(BENCH), "zstd", "1", &one);
    assert!(timed.status.success());
    let json: Value = serde_json::from_slice(&fs::read(one.join("ab.json")).unwrap()).unwrap();
    let results = json["decode"]["results"].as_object().unwrap();
    assert_eq!(results.keys().collect::<Vec<_>>(), ["zstd"]);
}

#[cfg(unix)]
#[test]
fn the_m35_ab_fails_when_the_base_harness_checks_fewer_bytes() {
    use std::os::unix::fs::PermissionsExt;
    let work = tempfile::tempdir().unwrap();
    let corpus = small_corpus(work.path());
    let base = work.path().join("short-base");
    fs::write(
        &base,
        format!(
            "#!/usr/bin/env python3\nimport json, subprocess, sys\n\
             done = subprocess.run([{BENCH:?}, *sys.argv[1:]])\n\
             if sys.argv[1] == 'decode':\n\
             \x20   row = json.load(open(sys.argv[-1]))\n\
             \x20   row['verified_bytes'] -= 1\n\
             \x20   json.dump(row, open(sys.argv[-1], 'w'))\n\
             sys.exit(done.returncode)\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
    let refused = ab(&corpus, &base, "zstd", "1", &work.path().join("ab"));
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "a short base decode passed");
    assert!(stderr.contains("zstd zipper@base verified"), "{stderr}");

    let few = Command::new(python())
        .arg(repository().join("scripts/m35_ab.py"))
        .args(["--harness", BENCH, "--base-harness", BENCH])
        .args(["--base-commit", "feedface", "--rounds", "7", "--corpus"])
        .arg(&corpus)
        .arg("--out")
        .arg(work.path().join("few"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&few.stderr);
    assert!(!few.status.success(), "7 rounds passed without --trial");
    assert!(stderr.contains("11 or more"), "{stderr}");
}
