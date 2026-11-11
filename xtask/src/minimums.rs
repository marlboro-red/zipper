use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{fs, path::Path, process::Command};

pub fn check(root: &Path) -> Result<()> {
    let metadata = cargo_metadata(root)?;
    let zipper = metadata["packages"]
        .as_array()
        .context("cargo metadata has no packages")?
        .iter()
        .find(|package| package["name"] == "zipper")
        .context("cargo metadata does not list zipper")?;
    let toolchain = zipper["rust_version"]
        .as_str()
        .context("Cargo.toml has no rust-version")?
        .to_owned();
    let pins = minimum_pins(zipper);
    ensure!(!pins.is_empty(), "zipper declares no dependency minimums");
    run(Command::new("rustup").args(["toolchain", "install", &toolchain, "--profile", "minimal"]))?;

    let work = tempfile::tempdir()?;
    fs::create_dir(work.path().join("src"))?;
    fs::write(
        work.path().join("Cargo.toml"),
        format!(
            "[package]\nname = \"minimum-consumer\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n\
             [dependencies]\nzipper = {{ path = {:?}, features = {:?} }}\n\n[workspace]\n",
            root.display().to_string(),
            features(zipper),
        ),
    )?;
    fs::write(work.path().join("src/main.rs"), "fn main() {}\n")?;
    let cargo = |args: &[&str]| {
        let mut command = Command::new("cargo");
        command
            .current_dir(work.path())
            .env_remove("RUSTUP_TOOLCHAIN")
            .arg(format!("+{toolchain}"))
            .args(args);
        command
    };
    run(&mut cargo(&["generate-lockfile", "--quiet"]))?;
    let mut unpinned = pins;
    while !unpinned.is_empty() {
        let mut refusals = Vec::new();
        let before = unpinned.len();
        unpinned.retain(|(name, version)| {
            match cargo(&["update", "--quiet", "--package", name, "--precise", version]).output() {
                Ok(output) if output.status.success() => false,
                Ok(output) => {
                    let why = String::from_utf8_lossy(&output.stderr);
                    refusals.push(format!("{name} {version}: {}", why.trim()));
                    true
                }
                Err(error) => {
                    refusals.push(format!("{name} {version}: {error}"));
                    true
                }
            }
        });
        ensure!(
            unpinned.len() < before || unpinned.is_empty(),
            "the minimums Cargo.toml declares cannot be resolved together on Rust {toolchain}:\n{}",
            refusals.join("\n")
        );
    }
    run(cargo(&["check", "--quiet"]).env("CARGO_TARGET_DIR", root.join("target/minimums")))
        .with_context(|| {
            format!("zipper with every dependency at its declared minimum does not build on Rust {toolchain}")
        })
}

fn cargo_metadata(root: &Path) -> Result<Value> {
    let output = Command::new("cargo")
        .current_dir(root)
        .args(["metadata", "--format-version", "1", "--no-deps", "--locked"])
        .output()?;
    ensure!(
        output.status.success(),
        "cargo metadata exited with {}",
        output.status
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn features(zipper: &Value) -> Vec<String> {
    zipper["features"]
        .as_object()
        .map(|features| {
            features
                .keys()
                .filter(|name| *name != "default")
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn minimum_pins(zipper: &Value) -> Vec<(String, String)> {
    zipper["dependencies"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|dependency| dependency["kind"].is_null())
        .filter(|dependency| applies_here(dependency["target"].as_str()))
        .filter_map(|dependency| {
            let name = dependency["name"].as_str()?;
            let version = dependency["req"].as_str()?.strip_prefix('^')?;
            (version.split('.').count() == 3).then(|| (name.to_owned(), version.to_owned()))
        })
        .collect()
}

fn applies_here(target: Option<&str>) -> bool {
    match target {
        None => true,
        Some("cfg(unix)") => cfg!(unix),
        Some("cfg(windows)") => cfg!(windows),
        Some(_) => false,
    }
}

fn run(command: &mut Command) -> Result<()> {
    let status = command.status()?;
    if !status.success() {
        bail!("{command:?} exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{applies_here, minimum_pins};
    use serde_json::json;

    #[test]
    fn each_normal_dependency_for_this_platform_is_pinned_to_its_declared_minimum() {
        let zipper = json!({"dependencies": [
            {"name": "anyhow", "req": "^1.0.104", "kind": null, "target": null},
            {"name": "libc", "req": "^0.2.168", "kind": null, "target": "cfg(unix)"},
            {"name": "windows-sys", "req": "^0.61.2", "kind": null, "target": "cfg(windows)"},
            {"name": "zstd", "req": "^0.14.3", "kind": "dev", "target": null},
            {"name": "loose", "req": "^1", "kind": null, "target": null},
        ]});
        let mut expected = vec![("anyhow".to_owned(), "1.0.104".to_owned())];
        if cfg!(unix) {
            expected.push(("libc".to_owned(), "0.2.168".to_owned()));
        }
        if cfg!(windows) {
            expected.push(("windows-sys".to_owned(), "0.61.2".to_owned()));
        }
        assert_eq!(minimum_pins(&zipper), expected);
        assert!(!applies_here(Some("cfg(target_os = \"redox\")")));
    }
}
