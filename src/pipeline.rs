//! Ordered, bounded lanes. Each worker owns one input slot and one output slot;
//! a slow worker cannot cause an unbounded reorder buffer. Closing receivers
//! cancels blocked sends, including when the producer or consumer fails.
use anyhow::{Result, anyhow, ensure};
use std::{sync::mpsc::sync_channel, thread};

pub(super) fn run<I: Send, O: Send, C>(
    workers: usize,
    produce: impl FnOnce(&mut dyn FnMut(I) -> Result<()>) -> Result<()> + Send,
    init: impl Fn() -> Result<C> + Sync,
    transform: impl Fn(&mut C, I) -> Result<O> + Sync,
    mut consume: impl FnMut(O) -> Result<()>,
) -> Result<()> {
    ensure!(workers > 0 && workers <= 64, "worker count must be 1–64");
    thread::scope(|scope| {
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        let mut handles = Vec::new();
        for _ in 0..workers {
            let (tx, rx) = sync_channel(1);
            let (out, results) = sync_channel(1);
            inputs.push(tx);
            outputs.push(results);
            let init = &init;
            let transform = &transform;
            handles.push(scope.spawn(move || {
                let mut state = match init() {
                    Ok(state) => state,
                    Err(e) => {
                        let _ = out.send(Err(e));
                        return;
                    }
                };
                while let Ok(job) = rx.recv() {
                    let result = transform(&mut state, job);
                    let failed = result.is_err();
                    if out.send(result).is_err() || failed {
                        break;
                    }
                }
            }));
        }
        let producer = scope.spawn(move || {
            let mut next = 0;
            produce(&mut |job| {
                inputs[next]
                    .send(job)
                    .map_err(|_| anyhow!("pipeline worker stopped"))?;
                next = (next + 1) % workers;
                Ok(())
            })
        });
        let consumed: Result<()> = (|| {
            let mut next = 0;
            while let Ok(result) = outputs[next].recv() {
                consume(result?)?;
                next = (next + 1) % workers;
            }
            Ok(())
        })();
        // Release blocked workers before joining any thread.
        drop(outputs);
        let produced = producer
            .join()
            .map_err(|_| anyhow!("reader thread panicked"));
        let mut worker_panic = false;
        for handle in handles {
            worker_panic |= handle.join().is_err();
        }
        consumed?;
        produced??;
        ensure!(!worker_panic, "codec worker panicked");
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordered_and_cancels_on_each_stage_failure() {
        for fail in 0..4 {
            let mut seen = Vec::new();
            let result = run(
                3,
                |emit| {
                    for n in 0..100 {
                        ensure!(fail != 1 || n != 17, "reader failure");
                        emit(n)?;
                    }
                    Ok(())
                },
                || Ok(()),
                |_, n| {
                    ensure!(fail != 2 || n != 17, "codec failure");
                    Ok(n)
                },
                |n| {
                    ensure!(fail != 3 || n != 17, "writer failure");
                    seen.push(n);
                    Ok(())
                },
            );
            if fail == 0 {
                assert_eq!(seen, (0..100).collect::<Vec<_>>());
            }
            assert_eq!(result.is_ok(), fail == 0);
        }
    }
}
