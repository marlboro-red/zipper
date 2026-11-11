use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
    process::Command,
};

const NATIVE_BUILDERS: &[&str] = &["cc", "cmake", "pkg-config", "bindgen", "vcpkg", "autotools"];
// EXTERNAL(cargo): these crates set `links` only so that one copy of them is in a build; they link no native library.
const LINKS_WITHOUT_NATIVE_CODE: &[&str] = &["rayon-core", "wasm_bindgen"];
const SYSTEM_LIBRARIES: &[&str] = &["/usr/lib/libSystem.B.dylib", "/usr/lib/libiconv.2.dylib"];

pub fn lint(root: &Path) -> Result<()> {
    let output = Command::new("cargo")
        .current_dir(root)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--all-features",
            "--locked",
        ])
        .output()?;
    ensure!(
        output.status.success(),
        "cargo metadata exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    let native = native_crates(&metadata, "zipper")?;
    for problem in &native {
        eprintln!("{problem}");
    }
    ensure!(
        native.is_empty(),
        "{} crate(s) in Zipper's build compile or link native code; Zipper builds from Rust only, \
         and a C library may be only a dev-dependency used as a test oracle",
        native.len()
    );
    Ok(())
}

pub fn non_system_libraries(otool: &str) -> Vec<String> {
    otool
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .filter(|library| !SYSTEM_LIBRARIES.contains(library))
        .map(str::to_owned)
        .collect()
}

fn native_crates(metadata: &Value, root_name: &str) -> Result<Vec<String>> {
    let packages: HashMap<&str, &Value> = metadata["packages"]
        .as_array()
        .context("cargo metadata has no packages")?
        .iter()
        .filter_map(|package| Some((package["id"].as_str()?, package)))
        .collect();
    let nodes: HashMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .context("cargo metadata has no resolve graph")?
        .iter()
        .filter_map(|node| Some((node["id"].as_str()?, node)))
        .collect();
    let name = |id: &str| packages[id]["name"].as_str().unwrap_or_default().to_owned();
    let root = packages
        .keys()
        .copied()
        .find(|id| name(id) == root_name)
        .context("cargo metadata does not list the root package")?;
    let mut seen = BTreeSet::new();
    let mut stack = vec![root];
    let mut problems = Vec::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let package = packages[id];
        if let Some(links) = package["links"].as_str()
            && !LINKS_WITHOUT_NATIVE_CODE.contains(&links)
        {
            problems.push(format!("{}: links the native library `{links}`", name(id)));
        }
        for dependency in nodes[id]["deps"].as_array().into_iter().flatten() {
            let kinds: Vec<&str> = dependency["dep_kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|kind| kind["kind"].as_str().unwrap_or("normal"))
                .collect();
            let target = dependency["pkg"].as_str().unwrap_or_default();
            if kinds.contains(&"build") && NATIVE_BUILDERS.contains(&name(target).as_str()) {
                problems.push(format!(
                    "{}: builds native code with `{}`",
                    name(id),
                    name(target)
                ));
            }
            if kinds.iter().any(|kind| *kind != "dev") {
                stack.push(target);
            }
        }
    }
    problems.sort();
    problems.dedup();
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::{native_crates, non_system_libraries};
    use serde_json::{Value, json};

    fn package(name: &str, links: Option<&str>) -> Value {
        json!({"id": name, "name": name, "links": links})
    }

    fn edge(to: &str, kind: Option<&str>) -> Value {
        json!({"pkg": to, "dep_kinds": [{"kind": kind}]})
    }

    #[test]
    fn a_native_library_in_the_build_is_named_and_one_only_in_tests_is_not() {
        let metadata = json!({
            "packages": [
                package("zipper", None),
                package("rayon-core", Some("rayon-core")),
                package("zstd-sys", Some("zstd")),
                package("cc", None),
                package("bzip2-sys", Some("bzip2")),
                package("ring", None),
            ],
            "resolve": {"nodes": [
                {"id": "zipper", "deps": [
                    edge("rayon-core", None),
                    edge("zstd-sys", None),
                    edge("bzip2-sys", Some("dev")),
                    edge("ring", None),
                ]},
                {"id": "rayon-core", "deps": []},
                {"id": "zstd-sys", "deps": [edge("cc", Some("build"))]},
                {"id": "cc", "deps": []},
                {"id": "bzip2-sys", "deps": [edge("cc", Some("build"))]},
                {"id": "ring", "deps": [edge("cc", Some("build"))]},
            ]},
        });
        assert_eq!(
            native_crates(&metadata, "zipper").unwrap(),
            [
                "ring: builds native code with `cc`",
                "zstd-sys: builds native code with `cc`",
                "zstd-sys: links the native library `zstd`",
            ]
        );
    }

    #[test]
    fn only_libraries_outside_the_system_are_named() {
        let otool = "target/release/zipper:\n\
                     \t/usr/lib/libiconv.2.dylib (compatibility version 7.0.0)\n\
                     \t/opt/homebrew/lib/libzstd.1.dylib (compatibility version 1.0.0)\n\
                     \t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0)\n";
        assert_eq!(
            non_system_libraries(otool),
            ["/opt/homebrew/lib/libzstd.1.dylib"]
        );
    }
}
