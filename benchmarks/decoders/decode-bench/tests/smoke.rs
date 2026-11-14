use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
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
