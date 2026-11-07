//! Real xar archives and flat macOS packages, written by `xar`, `pkgbuild`
//! and `productbuild`, extract to the tree their readers write. The
//! archives in `tests/data/xar-corpus` must match the trees recorded beside
//! them on every system. Where the tools are installed, freshly built
//! archives must match `7zz x -txar` and `xar -x` on this machine.
//!
//! The recorded trees come from `xar -x`, with the `[TOC].xml` that 7-Zip
//! writes. `ZIPPER_WRITE_XAR_CORPUS=1` rebuilds the archives and records.
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use zipper::native_xar::{ReadOptions, read_archive};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/xar-corpus");
const TOC_FILE: &str = "[TOC].xml";
const HARD_LINKS: [&str; 2] = ["a.txt", "folder/hard"];

fn tree(root: &Path) -> BTreeMap<String, String> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let kind = if entry.path_is_symlink() {
                format!("link:{}", fs::read_link(entry.path()).unwrap().display())
            } else if entry.file_type().is_dir() {
                "dir".to_owned()
            } else {
                Sha256::digest(fs::read(entry.path()).unwrap())
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect()
            };
            (name, kind)
        })
        .collect()
}

fn found(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool}")])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn run(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_source(root: &Path) {
    fs::create_dir_all(root.join("folder/deeper")).unwrap();
    fs::create_dir_all(root.join("empty folder")).unwrap();
    fs::create_dir_all(root.join("names")).unwrap();
    fs::write(root.join("a.txt"), "hello\n").unwrap();
    let text: String = (0..2000)
        .map(|n| format!("line {n} of the corpus\n"))
        .collect();
    fs::write(root.join("folder/deeper/text.txt"), text).unwrap();
    let noise: Vec<u8> = (0..40_000u32)
        .map(|n| (n.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    fs::write(root.join("folder/noise.bin"), noise).unwrap();
    fs::write(root.join("names/odd & <name> \u{e9}.txt"), "odd\n").unwrap();
    fs::write(root.join("empty"), "").unwrap();
    fs::write(root.join("folder/run.sh"), "#!/bin/sh\necho hi\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            root.join("folder/run.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::os::unix::fs::symlink("a.txt", root.join("link")).unwrap();
    }
    fs::hard_link(root.join("a.txt"), root.join("folder/hard")).unwrap();
}

fn write_package_source(root: &Path) {
    fs::create_dir_all(root.join("usr/local/share/zipper-test")).unwrap();
    fs::write(
        root.join("usr/local/share/zipper-test/readme.txt"),
        "package\n",
    )
    .unwrap();
    fs::write(
        root.join("usr/local/share/zipper-test/data.txt"),
        "data\n".repeat(500),
    )
    .unwrap();
}

fn build_corpus_with_installed_tools(work: &Path) -> Vec<PathBuf> {
    let source = work.join("source");
    write_source(&source);
    let mut made = Vec::new();
    if found("xar") {
        for (name, flags) in [
            ("gzip.xar", &[][..]),
            ("bzip2.xar", &["--compression=bzip2"][..]),
            (
                "stored-sha512.xar",
                &[
                    "--compression=none",
                    "--toc-cksum=sha512",
                    "--file-cksum=sha256",
                ][..],
            ),
            (
                "gzip-sha256.xar",
                &["--toc-cksum=sha256", "--file-cksum=sha512"][..],
            ),
        ] {
            let out = work.join(name);
            run(Command::new("xar")
                .arg("-cf")
                .arg(&out)
                .args(flags)
                .arg(".")
                .current_dir(&source));
            made.push(out);
        }
    }
    if found("pkgbuild") && found("productbuild") {
        let root = work.join("package-root");
        write_package_source(&root);
        let scripts = work.join("scripts");
        fs::create_dir_all(&scripts).unwrap();
        fs::write(scripts.join("postinstall"), "#!/bin/sh\nexit 0\n").unwrap();
        let component = work.join("component.pkg");
        run(Command::new("pkgbuild")
            .arg("--root")
            .arg(&root)
            .args(["--identifier", "com.example.zipper", "--version", "1.0"])
            .args(["--install-location", "/"])
            .arg(&component));
        let scripted = work.join("scripted.pkg");
        run(Command::new("pkgbuild")
            .arg("--root")
            .arg(&root)
            .args([
                "--identifier",
                "com.example.zipper.scripted",
                "--version",
                "2.0",
            ])
            .args(["--compression", "latest", "--min-os-version", "12.0"])
            .arg("--scripts")
            .arg(&scripts)
            .arg(&scripted));
        let distribution = work.join("distribution.xml");
        run(Command::new("productbuild")
            .args(["--synthesize", "--package"])
            .arg(&component)
            .arg("--package")
            .arg(&scripted)
            .arg(&distribution));
        let product = work.join("product.pkg");
        run(Command::new("productbuild")
            .arg("--distribution")
            .arg(&distribution)
            .arg("--package-path")
            .arg(work)
            .arg(&product));
        made.extend([component, scripted, product]);
    }
    made
}

fn zipper_tree(archive: &Path, into: &Path) -> BTreeMap<String, String> {
    read_archive(archive, Some(into), ReadOptions::default())
        .unwrap_or_else(|error| panic!("{}: {error:#}", archive.display()));
    tree(into)
}

fn sevenzip_tree(archive: &Path, into: &Path) -> BTreeMap<String, String> {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    run(Command::new("7zz")
        .args(["x", "-y", "-txar"])
        .arg(flag)
        .arg(archive));
    tree(into)
}

fn xar_tree(archive: &Path, into: &Path) -> BTreeMap<String, String> {
    fs::create_dir_all(into).unwrap();
    run(Command::new("xar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into));
    tree(into)
}

fn without(mut tree: BTreeMap<String, String>, names: &[&str]) -> BTreeMap<String, String> {
    for name in names {
        tree.remove(*name);
    }
    tree
}

#[test]
fn freshly_built_archives_match_7zip_and_xar() {
    if !found("7zz") || !found("xar") {
        eprintln!("7zz or xar is not installed; skipped");
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let archives = build_corpus_with_installed_tools(work.path());
    assert!(!archives.is_empty(), "no corpus tool is installed");
    let mut records = serde_json::Map::new();
    for archive in &archives {
        let name = archive.file_name().unwrap().to_string_lossy().into_owned();
        let ours = zipper_tree(archive, &work.path().join(format!("zipper-{name}")));
        let sevenzip = sevenzip_tree(archive, &work.path().join(format!("7zz-{name}")));
        let xar = xar_tree(archive, &work.path().join(format!("xar-{name}")));
        assert_eq!(
            without(ours.clone(), &HARD_LINKS),
            without(sevenzip.clone(), &HARD_LINKS),
            "{name} against 7zz"
        );
        assert_eq!(
            without(ours.clone(), &[TOC_FILE]),
            xar,
            "{name} against xar"
        );
        let mut recorded = xar;
        recorded.insert(TOC_FILE.to_owned(), sevenzip[TOC_FILE].clone());
        records.insert(name, serde_json::to_value(recorded).unwrap());
    }
    if std::env::var_os("ZIPPER_WRITE_XAR_CORPUS").is_some() {
        let corpus = Path::new(CORPUS);
        fs::create_dir_all(corpus).unwrap();
        for archive in &archives {
            fs::copy(archive, corpus.join(archive.file_name().unwrap())).unwrap();
        }
        let manifest = serde_json::to_string_pretty(&records).unwrap() + "\n";
        fs::write(corpus.join("trees.json"), manifest).unwrap();
    }
}

#[test]
fn recorded_archives_extract_to_their_recorded_trees() {
    let manifest: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&fs::read_to_string(Path::new(CORPUS).join("trees.json")).unwrap())
            .unwrap();
    assert!(manifest.len() >= 7, "{}", manifest.len());
    for (name, recorded) in manifest {
        let recorded: BTreeMap<String, String> = serde_json::from_value(recorded).unwrap();
        if !cfg!(unix) && recorded.values().any(|kind| kind.starts_with("link:")) {
            continue;
        }
        let work = tempfile::tempdir().unwrap();
        let ours = zipper_tree(&Path::new(CORPUS).join(&name), &work.path().join("out"));
        assert_eq!(ours, recorded, "{name}");
    }
}
