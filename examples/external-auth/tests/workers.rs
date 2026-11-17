use external_auth::Workers;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

#[test]
fn reservations_and_finished_handles_stay_charged_until_explicit_reaping() {
    let workers = Workers::default();
    let permits: Vec<_> = (0..4).map(|_| workers.reserve().unwrap()).collect();
    assert!(workers.reserve().is_none());
    assert_eq!(workers.retained(), 4);
    let done = Arc::new(AtomicUsize::new(0));
    for permit in permits {
        let done = done.clone();
        permit.start(move |_| {
            done.fetch_add(1, Ordering::Release);
        });
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while done.load(Ordering::Acquire) != 4 && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(done.load(Ordering::Acquire), 4);
    assert!(!workers.quiescent());
    assert!(workers.reserve().is_none());
    while !workers.quiescent() && Instant::now() < deadline {
        workers.reap_finished();
        std::thread::yield_now();
    }
    assert!(workers.quiescent());
    assert!(workers.reserve().is_some());
}
#[test]
fn abandoned_starter_release_and_cancellation_are_owned_and_nonblocking() {
    let workers = Workers::default();
    let reservation = workers.reserve().unwrap();
    assert_eq!(workers.retained(), 1);
    drop(reservation);
    assert!(workers.quiescent());
    let barrier = Arc::new(Barrier::new(2));
    let worker_barrier = barrier.clone();
    workers.reserve().unwrap().start(move |cancelled| {
        worker_barrier.wait();
        while !cancelled.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
    });
    barrier.wait();
    workers.reap_finished();
    assert_eq!(workers.retained(), 1);
    workers.cancel();
    assert!(workers.reserve().is_none());
    let deadline = Instant::now() + Duration::from_secs(2);
    while !workers.quiescent() && Instant::now() < deadline {
        workers.reap_finished();
        std::thread::yield_now();
    }
    assert!(workers.quiescent());
}
#[test]
fn worker_panic_latches_unload_refusal_after_join() {
    let workers = Workers::default();
    workers
        .reserve()
        .unwrap()
        .start(|_| panic!("fixed test worker panic"));
    let deadline = Instant::now() + Duration::from_secs(2);
    while workers.retained() != 0 && Instant::now() < deadline {
        workers.reap_finished();
        std::thread::yield_now();
    }
    assert_eq!(workers.retained(), 0);
    assert!(!workers.quiescent());
    assert!(workers.reserve().is_none());
}
