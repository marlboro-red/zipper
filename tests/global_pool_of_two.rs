use std::io::Write;
use zipper::{Control, Progress, native_zip};

fn pin_global_pool_to_two_threads() {
    rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build_global()
        .unwrap();
}

#[test]
fn a_callback_that_runs_rayon_work_finishes_while_extractions_fill_the_pool() {
    use rayon::prelude::*;
    pin_global_pool_to_two_threads();
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.zip");
    let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
    writer
        .start_file(
            "large",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored)
                .large_file(false),
        )
        .unwrap();
    let block: Vec<u8> = (0..1 << 20).map(|index| (index * 31 % 251) as u8).collect();
    for _ in 0..64 {
        writer.write_all(&block).unwrap();
    }
    writer.finish().unwrap();
    let destinations: Vec<_> = (0..2)
        .map(|index| root.path().join(format!("out{index}")))
        .collect();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let joins = |_: Progress| {
            rayon::join(|| (), || ());
        };
        let results: Vec<_> = destinations
            .par_iter()
            .map(|destination| {
                native_zip::read_archive_with_options(
                    &archive,
                    Some(destination),
                    native_zip::ReadOptions {
                        control: Control {
                            progress: Some(&joins),
                            cancel: None,
                        },
                        ..Default::default()
                    },
                )
                .map(|entries| entries.len())
                .map_err(|error| error.to_string())
            })
            .collect();
        let _ = done.send(results);
    });
    let results = finished
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("a writer thread's callback waited for a pool its extractions filled");
    assert_eq!(results, vec![Ok(1), Ok(1)]);
}
