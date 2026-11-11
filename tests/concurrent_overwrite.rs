//! Several `Existing::Overwrite` extractions at once into one destination,
//! from archives of plain files and folders that share folder names.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Barrier,
    time::Duration,
};
use zip::write::SimpleFileOptions;
use zipper::{Existing, native_zip};

type Tree = BTreeMap<PathBuf, Option<Vec<u8>>>;

fn setting(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn content(archive: usize, path: &str, length: usize) -> Vec<u8> {
    let seed = format!("{archive}:{path}:");
    seed.bytes().cycle().take(length.max(seed.len())).collect()
}

struct Archive {
    path: PathBuf,
    files: Tree,
}

fn members(archive: usize, files: usize) -> Vec<(String, Vec<u8>)> {
    let mut members = Vec::new();
    for index in 0..files {
        let path = match index % 5 {
            0 => format!("bin/tool_{archive}_{index}"),
            1 => format!("lib/lib_{archive}_{index}.so"),
            2 => format!("lib/python/module_{archive}_{index}.py"),
            3 => format!("share/doc/archive_{archive}/page_{index}"),
            _ => format!("share/locale/{}/messages_{archive}_{index}", index % 3),
        };
        let length = (index * 37 + archive * 101) % 9000;
        members.push((path.clone(), content(archive, &path, length)));
    }
    members.push((format!("readme_{archive}"), content(archive, "readme", 300)));
    members
}

fn write_archive(folder: &Path, archive: usize, members: &[(String, Vec<u8>)]) -> Archive {
    let path = folder.join(format!("part_{archive}.zip"));
    let mut writer = zip::ZipWriter::new(fs::File::create(&path).unwrap());
    let method = if archive.is_multiple_of(2) {
        zip::CompressionMethod::Stored
    } else {
        zip::CompressionMethod::Deflated
    };
    if archive.is_multiple_of(3) {
        for folder in ["bin/", "lib/", "lib/python/", "share/"] {
            writer
                .add_directory(folder, SimpleFileOptions::default().unix_permissions(0o755))
                .unwrap();
        }
    }
    for (name, data) in members {
        writer
            .start_file(
                name.as_str(),
                SimpleFileOptions::default()
                    .compression_method(method)
                    .unix_permissions(0o644),
            )
            .unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap();
    let files = members
        .iter()
        .map(|(name, data)| (PathBuf::from(name), Some(data.clone())))
        .collect();
    Archive { path, files }
}

fn damage_last_file(archive: &Archive) {
    let mut bytes = fs::read(&archive.path).unwrap();
    let (_, last) = archive.files.iter().next_back().unwrap();
    let last = last.as_ref().unwrap();
    let marker = &last[..last.len().min(16)];
    let at = bytes
        .windows(marker.len())
        .rposition(|window| window == marker)
        .expect("the last file is stored");
    bytes[at] ^= 0x55;
    fs::write(&archive.path, bytes).unwrap();
}

fn files_on_disk(root: &Path) -> Tree {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(|entry| entry.unwrap())
        .filter(|entry| !entry.file_type().is_dir())
        .map(|entry| {
            let path = entry.path().strip_prefix(root).unwrap().to_owned();
            let content = entry
                .file_type()
                .is_file()
                .then(|| fs::read(entry.path()).unwrap());
            (path, content)
        })
        .collect()
}

fn leftovers(root: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".zipper-"))
        .map(|entry| entry.path().to_owned())
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum Start {
    Empty,
    WithSharedFolders,
    Missing,
}

fn prepare(destination: &Path, start: Start) {
    match start {
        Start::Missing => {}
        Start::Empty => fs::create_dir(destination).unwrap(),
        Start::WithSharedFolders => {
            fs::create_dir_all(destination.join("bin")).unwrap();
            fs::create_dir_all(destination.join("lib/python")).unwrap();
        }
    }
}

fn extract_all(
    archives: &[Archive],
    destination: &Path,
    round: usize,
) -> Vec<anyhow::Result<Vec<zipper::Entry>>> {
    let barrier = Barrier::new(archives.len());
    std::thread::scope(|scope| {
        let handles: Vec<_> = archives
            .iter()
            .enumerate()
            .map(|(index, archive)| {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    let delay = (index * 7919 + round * 104_729) % 5;
                    std::thread::sleep(Duration::from_millis(delay as u64));
                    native_zip::read_archive_with_options(
                        &archive.path,
                        Some(destination),
                        native_zip::ReadOptions {
                            existing: Existing::Overwrite,
                            threads: Some(1 + (index + round) % 3),
                            ..Default::default()
                        },
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    })
}

const STARTS: [Start; 3] = [Start::Empty, Start::WithSharedFolders, Start::Missing];

#[test]
fn disjoint_files_in_shared_folders_extract_together_byte_exact() {
    let rounds = setting("ZIPPER_CONCURRENT_ROUNDS", 6);
    let count = setting("ZIPPER_CONCURRENT_ARCHIVES", 8);
    let source = tempfile::tempdir().unwrap();
    let archives: Vec<Archive> = (0..count)
        .map(|archive| write_archive(source.path(), archive, &members(archive, 120)))
        .collect();
    let mut expected = Tree::new();
    for archive in &archives {
        expected.extend(archive.files.clone());
    }
    for round in 0..rounds {
        let start = STARTS[round % STARTS.len()];
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("out");
        prepare(&destination, start);
        let results = extract_all(&archives, &destination, round);
        for (index, result) in results.iter().enumerate() {
            if let Err(error) = result {
                panic!("round {round} from {start:?}: archive {index} failed: {error:#}");
            }
        }
        assert_eq!(
            leftovers(root.path()),
            Vec::<PathBuf>::new(),
            "round {round}"
        );
        let actual = files_on_disk(&destination);
        assert_eq!(
            actual.keys().collect::<Vec<_>>(),
            expected.keys().collect::<Vec<_>>(),
            "round {round} from {start:?}"
        );
        assert!(actual == expected, "round {round}: some bytes differ");
    }
}

#[test]
fn a_damaged_archive_fails_alone_and_the_others_publish() {
    let rounds = setting("ZIPPER_CONCURRENT_ROUNDS", 6);
    let count = setting("ZIPPER_CONCURRENT_ARCHIVES", 8);
    let source = tempfile::tempdir().unwrap();
    let archives: Vec<Archive> = (0..count)
        .map(|archive| write_archive(source.path(), archive, &members(archive, 120)))
        .collect();
    let damaged = count / 2;
    let stored = damaged - damaged % 2;
    damage_last_file(&archives[stored]);
    let mut expected = Tree::new();
    for (index, archive) in archives.iter().enumerate() {
        if index != stored {
            expected.extend(archive.files.clone());
        }
    }
    for round in 0..rounds {
        let start = STARTS[round % STARTS.len()];
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("out");
        prepare(&destination, start);
        let results = extract_all(&archives, &destination, round);
        for (index, result) in results.iter().enumerate() {
            match result {
                Err(error) if index != stored => {
                    panic!("round {round} from {start:?}: archive {index} failed: {error:#}")
                }
                Ok(_) if index == stored => panic!("round {round}: the damaged archive passed"),
                _ => {}
            }
        }
        assert_eq!(
            leftovers(root.path()),
            Vec::<PathBuf>::new(),
            "round {round}"
        );
        assert!(
            files_on_disk(&destination) == expected,
            "round {round} from {start:?}: the published files differ"
        );
    }
}

#[test]
fn a_file_two_archives_share_ends_whole_from_one_of_them() {
    let rounds = setting("ZIPPER_CONCURRENT_ROUNDS", 6) * 2;
    let source = tempfile::tempdir().unwrap();
    let shared = "bin/shared";
    let archives: Vec<Archive> = (0..4)
        .map(|archive| {
            let mut members = members(archive, 40);
            members.insert(0, (shared.to_owned(), content(archive, shared, 1 << 20)));
            write_archive(source.path(), archive, &members)
        })
        .collect();
    let versions: Vec<&Vec<u8>> = archives
        .iter()
        .map(|archive| archive.files[Path::new(shared)].as_ref().unwrap())
        .collect();
    for round in 0..rounds {
        let start = STARTS[round % STARTS.len()];
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("out");
        prepare(&destination, start);
        if round % 2 == 1 && !matches!(start, Start::Missing) {
            fs::create_dir_all(destination.join("bin")).unwrap();
            fs::write(destination.join(shared), b"old").unwrap();
        }
        let results = extract_all(&archives, &destination, round);
        assert!(
            results.iter().any(Result::is_ok),
            "round {round}: every archive failed"
        );
        let actual = files_on_disk(&destination);
        let published = actual[Path::new(shared)].as_ref().unwrap();
        assert!(
            versions.contains(&published),
            "round {round} from {start:?}: {shared} mixes archives"
        );
        for (index, (archive, result)) in archives.iter().zip(&results).enumerate() {
            for (path, bytes) in &archive.files {
                if path == Path::new(shared) {
                    continue;
                }
                match result {
                    Ok(_) => assert_eq!(actual.get(path), Some(bytes), "round {round}"),
                    Err(error) => {
                        let message = format!("{error:#}");
                        assert!(
                            message.contains("appeared in the destination"),
                            "round {round}: archive {index} failed: {message}"
                        );
                        assert_eq!(actual.get(path), None, "round {round}: {message}");
                    }
                }
            }
        }
        assert_eq!(
            leftovers(root.path()),
            Vec::<PathBuf>::new(),
            "round {round}"
        );
    }
}
