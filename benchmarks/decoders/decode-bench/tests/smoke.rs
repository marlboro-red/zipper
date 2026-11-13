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
