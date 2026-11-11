use anyhow::{Result, ensure};
use std::{path::Path, process::Command};

const SETTINGS: [&str; 3] = ["zipper.remoteHost", "zipper.remoteGit", "zipper.remoteDir"];

pub fn lint(root: &Path) -> Result<()> {
    let settings: Vec<String> = SETTINGS
        .iter()
        .filter_map(|key| setting(root, key))
        .collect();
    let private = private_strings(&settings);
    if private.is_empty() {
        return Ok(());
    }
    let mut grep = Command::new("git");
    grep.current_dir(root).args(["grep", "-I", "-l", "-F"]);
    for text in &private {
        grep.args(["-e", text]);
    }
    let output = grep.output()?;
    ensure!(
        matches!(output.status.code(), Some(0 | 1)),
        "git grep exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let files = String::from_utf8_lossy(&output.stdout);
    let files: Vec<&str> = files.lines().collect();
    for file in &files {
        eprintln!("{file}: names the second machine's host, user or folder");
    }
    ensure!(
        files.is_empty(),
        "{} tracked file(s) name the second machine set by `git config {}`; \
         write paths relative to its checkout instead",
        files.len(),
        SETTINGS.join("`, `")
    );
    Ok(())
}

fn setting(root: &Path, key: &str) -> Option<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["config", "--get", key])
        .output()
        .ok()?;
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (output.status.success() && !value.is_empty()).then_some(value)
}

fn private_strings(settings: &[String]) -> Vec<String> {
    let mut private = Vec::new();
    for value in settings {
        private.push(value.clone());
        if let Some(home) = home_folder(value) {
            private.push(home);
        }
    }
    private.sort();
    private.dedup();
    private
}

fn home_folder(text: &str) -> Option<String> {
    ["/Users/", "/home/"].iter().find_map(|base| {
        let start = text.find(base)?;
        let user = text[start + base.len()..].split('/').next()?;
        (!user.is_empty()).then(|| format!("{base}{user}/"))
    })
}

#[cfg(test)]
mod tests {
    use super::{home_folder, private_strings};

    #[test]
    fn a_setting_and_the_home_folder_inside_it_are_both_private() {
        assert_eq!(
            home_folder("/Users/someone/work/checkout").as_deref(),
            Some("/Users/someone/")
        );
        assert_eq!(
            home_folder("box:/home/other/checkout.git").as_deref(),
            Some("/home/other/")
        );
        assert_eq!(home_folder("box"), None);
        assert_eq!(home_folder("/Users/"), None);
        let settings = ["box".to_owned(), "/Users/someone/work".to_owned()];
        assert_eq!(
            private_strings(&settings),
            ["/Users/someone/", "/Users/someone/work", "box"]
        );
    }
}
