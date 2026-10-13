//! Ordered, bounded worker windows. Only the coordinator calls host callbacks.

use crate::max_worker_threads;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::sync_channel,
    Mutex,
};

/// Maximum source blocks per decode task. Dense inputs are borrowed; packed tasks
/// own at most 128 KiB of words and 256 KiB of output. Eight admitted tasks add at
/// most 3 MiB of payload, excluding allocator and stack overhead. No full packed
/// region or decompressed document is retained.
pub(crate) const BATCH_BLOCKS: usize = 16 * 1024;

pub(crate) fn check_cancelled(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Relaxed) {
        Err("Parallel preview cancelled.".into())
    } else {
        Ok(())
    }
}

/// Persistent scoped workers each own one task/result slot. Memory-first mode
/// drains a complete window before refilling; speed-first mode refills each slot
/// immediately after its ordered result is consumed. Both admit at most
/// `workers` tasks, including finished but unconsumed results. Every exit closes
/// channels and joins workers; coordinator callbacks deliberately need not be Sync.
pub(crate) fn ordered<T: Send, R: Send>(
    mut jobs: impl Iterator<Item = Result<T, String>>,
    workers: usize,
    speed_first: bool,
    work: impl Fn(T, &AtomicBool) -> Result<R, String> + Sync,
    mut consume: impl FnMut(R) -> Result<(), String>,
    current: &impl Fn() -> Result<(), String>,
) -> Result<(), String> {
    let workers = workers.clamp(1, 8).min(jobs.size_hint().1.unwrap_or(8));
    if workers == 0 {
        return current();
    }
    let cancelled = AtomicBool::new(false);
    let worker_error = Mutex::new(None::<String>);
    std::thread::scope(|scope| {
        let mut inputs = Vec::with_capacity(workers);
        let mut outputs = Vec::with_capacity(workers);
        let mut handles = Vec::with_capacity(workers);
        let result = (|| {
            for _ in 0..workers {
                current()?;
                let (input, receive) = sync_channel::<T>(1);
                let (send, output) = sync_channel(1);
                let work = &work;
                let cancelled = &cancelled;
                let worker_error = &worker_error;
                let handle = std::thread::Builder::new()
                    .name("preview-compute".into())
                    .spawn_scoped(scope, move || {
                        while let Ok(job) = receive.recv() {
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    check_cancelled(cancelled)?;
                                    work(job, cancelled)
                                }))
                                .unwrap_or_else(|_| {
                                    Err("A parallel preview worker panicked.".into())
                                });
                            if let Err(error) = &result {
                                worker_error
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .get_or_insert_with(|| error.clone());
                                cancelled.store(true, Ordering::Relaxed);
                            }
                            if send.send(result).is_err() {
                                break;
                            }
                        }
                    })
                    .map_err(|error| format!("Unable to start preview worker: {error}"))?;
                inputs.push(input);
                outputs.push(output);
                handles.push(handle);
            }
            let mut admit = |input: &std::sync::mpsc::SyncSender<T>| {
                current()?;
                if cancelled.load(Ordering::Relaxed) {
                    return Ok(false);
                }
                let Some(job) = jobs.next() else {
                    return Ok(false);
                };
                let job = job?;
                if cancelled.load(Ordering::Relaxed) {
                    return Ok(false);
                }
                input
                    .send(job)
                    .map_err(|_| "A parallel preview worker stopped.".to_string())?;
                Ok::<_, String>(true)
            };
            let mut admitted = 0;
            let mut exhausted = false;
            loop {
                if admitted == 0 {
                    for input in &inputs {
                        if !admit(input)? {
                            break;
                        }
                        admitted += 1;
                    }
                }
                if admitted == 0 {
                    return worker_error
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .map_or(Ok(()), Err);
                }
                let mut refilled = 0;
                for (slot, output) in outputs.iter().take(admitted).enumerate() {
                    let result = (|| loop {
                        match output.recv_timeout(std::time::Duration::from_millis(100)) {
                            Ok(result) => break Ok(result),
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                                current()?;
                                if let Some(error) = worker_error
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .clone()
                                {
                                    return Err(error);
                                }
                            }
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                                return Err("A parallel preview worker stopped.".into());
                            }
                        }
                    })();
                    let result = result?;
                    current()?;
                    if let Some(error) = worker_error
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                    {
                        return Err(error);
                    }
                    consume(result?)?;
                    if speed_first && !exhausted {
                        if admit(&inputs[slot])? {
                            refilled += 1;
                        } else {
                            exhausted = true;
                        }
                    }
                }
                if speed_first && refilled == 0 {
                    return worker_error
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .map_or(Ok(()), Err);
                }
                admitted = refilled;
            }
        })();
        cancelled.store(true, Ordering::Relaxed);
        // Dropping receivers releases senders blocked after a callback failure.
        // No further work is queued, so shutdown does not need to drain tasks.
        drop(inputs);
        drop(outputs);
        let mut result = result;
        for handle in handles {
            if handle.join().is_err() && result.is_ok() {
                result = Err("A parallel preview worker panicked.".into());
            }
        }
        result
    })
}

/// Applies `task` to every item over as many threads as the machine allows,
/// each carrying its own freshly initialized `State`, and returns results in
/// input order. For short, pure per-item work over a shared read-only input
/// where a per-item construction would repeat one pack-wide lookup table —
/// the atlas phase's per-palette-state verdicts are the whole caller set.
/// Small inputs run inline: thread startup would dominate the work.
pub(crate) fn shard_map<T: Sync, S, R: Send>(
    items: &[T],
    init: impl Fn() -> S + Sync,
    task: impl Fn(&mut S, &T) -> R + Sync,
) -> Result<Vec<R>, String> {
    const INLINE_LIMIT: usize = 512;
    let cap = max_worker_threads() as usize;
    let shards = cap.min(items.len());
    if shards <= 1 || items.len() < INLINE_LIMIT {
        let mut state = init();
        return Ok(items.iter().map(|item| task(&mut state, item)).collect());
    }
    let width = items.len().div_ceil(shards);
    // Handles are pushed in shard order, so joining in order and flattening
    // keeps the output deterministic regardless of which thread finishes first.
    std::thread::scope(|scope| {
        let init = &init;
        let task = &task;
        let mut handles = Vec::with_capacity(shards);
        for shard in items.chunks(width) {
            let handle = std::thread::Builder::new()
                .name("preview-compute".into())
                .spawn_scoped(scope, move || {
                    let mut state = init();
                    shard
                        .iter()
                        .map(|item| task(&mut state, item))
                        .collect::<Vec<R>>()
                })
                .map_err(|error| format!("Unable to start preview worker: {error}"))?;
            handles.push(handle);
        }
        let mut output = Vec::with_capacity(items.len());
        for handle in handles {
            let shard_results = handle
                .join()
                .map_err(|_| "A parallel preview worker panicked.".to_string())?;
            output.extend(shard_results);
        }
        Ok(output)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::{atomic::AtomicUsize, Barrier};

    #[test]
    fn workers_compute_concurrently_but_publish_in_source_order() {
        let barrier = Barrier::new(2);
        let coordinator = std::thread::current().id();
        let callbacks = Cell::new(0);
        let mut actual = Vec::new();
        ordered(
            (0..6).map(Ok),
            2,
            false,
            |value, _| {
                assert_ne!(std::thread::current().id(), coordinator);
                barrier.wait();
                Ok(value * value)
            },
            |value| {
                actual.push(value);
                Ok(())
            },
            &|| {
                assert_eq!(std::thread::current().id(), coordinator);
                callbacks.set(callbacks.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(actual, [0, 1, 4, 9, 16, 25]);
    }

    #[test]
    fn cancellation_and_consumer_failure_stop_admission_and_drop_stale_results() {
        struct Tracked<'a>(&'a AtomicUsize);
        impl Drop for Tracked<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        for (speed_first, cancel) in [(false, false), (false, true), (true, false), (true, true)] {
            let live = AtomicUsize::new(0);
            let admitted = Cell::new(0);
            let consumed = Cell::new(false);
            let jobs = (0..20).map(|value| {
                admitted.set(admitted.get() + 1);
                Ok(value)
            });
            let result = ordered(
                jobs,
                2,
                speed_first,
                |_, _| {
                    live.fetch_add(1, Ordering::SeqCst);
                    Ok(Tracked(&live))
                },
                |_| {
                    consumed.set(true);
                    if cancel {
                        Ok(())
                    } else {
                        Err("consumer stopped".into())
                    }
                },
                &|| {
                    if cancel && consumed.get() {
                        Err("cancelled by caller".into())
                    } else {
                        Ok(())
                    }
                },
            );
            assert_eq!(
                result.err().as_deref(),
                Some(if cancel {
                    "cancelled by caller"
                } else {
                    "consumer stopped"
                })
            );
            assert_eq!(admitted.get(), 2);
            assert_eq!(live.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn worker_panics_and_errors_are_recoverable_and_joined() {
        for (speed_first, panic) in [(false, false), (false, true), (true, false), (true, true)] {
            let result = ordered(
                (0..4).map(Ok),
                2,
                speed_first,
                |_, _| -> Result<(), String> {
                    if panic {
                        panic!("broken mesher")
                    }
                    Err("invalid packed state".into())
                },
                |_| panic!("failed task must not publish"),
                &|| Ok(()),
            );
            assert_eq!(
                result.err().as_deref(),
                Some(if panic {
                    "A parallel preview worker panicked."
                } else {
                    "invalid packed state"
                })
            );
        }
    }

    #[test]
    fn speed_first_refills_consumed_slot_before_slow_sibling_finishes() {
        let (release, blocked) = sync_channel(1);
        let blocked = Mutex::new(blocked);
        let admitted = Cell::new(0usize);
        let consumed = Cell::new(0usize);
        let coordinator = std::thread::current().id();
        let mut published = Vec::new();
        ordered(
            (0..7).map(|value| {
                admitted.set(admitted.get() + 1);
                assert!(admitted.get() - consumed.get() <= 2);
                Ok(value)
            }),
            2,
            true,
            |value, _| {
                assert_ne!(std::thread::current().id(), coordinator);
                if value == 1 {
                    blocked
                        .lock()
                        .unwrap()
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .map_err(|_| "consumed worker slot was not refilled".to_string())?;
                } else if value == 2 {
                    release.send(()).unwrap();
                }
                Ok(value)
            },
            |value| {
                assert_eq!(std::thread::current().id(), coordinator);
                published.push(value);
                consumed.set(consumed.get() + 1);
                Ok(())
            },
            &|| {
                assert_eq!(std::thread::current().id(), coordinator);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(published, [0, 1, 2, 3, 4, 5, 6]);
    }
}
