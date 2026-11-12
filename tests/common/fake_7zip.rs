#![allow(dead_code)]
use std::{
    ffi::{OsStr, OsString},
    fs,
    path::Path,
    process::{Command, Stdio},
};

pub struct Fake7Zip {
    folder: tempfile::TempDir,
    first_name: String,
}

pub struct Run {
    pub success: bool,
    pub stderr: String,
}

impl Fake7Zip {
    pub fn new(names_on_path: &[&str]) -> Fake7Zip {
        let folder = tempfile::tempdir().unwrap();
        let marker = folder.path().join("started");
        for name in names_on_path {
            let script = folder.path().join(name);
            fs::write(
                &script,
                format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 0\n", marker.display()),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        Fake7Zip {
            folder,
            first_name: names_on_path[0].to_owned(),
        }
    }

    pub fn started(&self) -> Option<String> {
        fs::read_to_string(self.folder.path().join("started")).ok()
    }

    fn path(&self) -> OsString {
        let mut path = self.folder.path().as_os_str().to_owned();
        if let Some(rest) = std::env::var_os("PATH") {
            path.push(":");
            path.push(rest);
        }
        path
    }

    pub fn zipper<S: AsRef<OsStr>>(&self, args: &[S]) -> Run {
        let output = Command::new(env!("CARGO_BIN_EXE_zipper"))
            .args(args)
            .env("PATH", self.path())
            .env("ZIPPER_7Z", self.folder.path().join(&self.first_name))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        Run {
            success: output.status.success(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    pub fn assert_refused(&self, archive: &Path, words: &str) {
        self.assert_refused_by(&["extract", "list", "test"], archive, words);
    }

    pub fn assert_refused_by(&self, actions: &[&str], archive: &Path, words: &str) {
        let output = archive.with_extension("out");
        for &action in actions {
            let mut args: Vec<&OsStr> = vec![action.as_ref(), archive.as_os_str()];
            if action == "extract" {
                args.extend(["-o".as_ref(), output.as_os_str()]);
            }
            let run = self.zipper(&args);
            let label = format!("{action} {}", archive.display());
            assert_eq!(self.started(), None, "{label} started 7-Zip");
            assert!(!run.success, "{label} succeeded");
            assert!(
                run.stderr.contains(words),
                "{label}: wanted {words:?}, got {}",
                run.stderr
            );
            assert!(!output.exists(), "{label} wrote output");
        }
    }
}
