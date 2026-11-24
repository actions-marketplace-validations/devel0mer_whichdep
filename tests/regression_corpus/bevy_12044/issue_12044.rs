use bevy_tasks::{TaskPoolBuilder, block_on};
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn bevy_12044_async_executor_1_9_0_deadlock() {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let pool = TaskPoolBuilder::new().num_threads(1).build();
        // First scope initializes this thread's ThreadLocal local_queue in async-executor 1.9.0.
        pool.scope(|s| {
            s.spawn(async {
                futures_lite::future::yield_now().await;
            });
        });
        // Now spawn a task from this same thread (which is no longer polling the executor)
        // and block_on it. In async-executor 1.9.0 the task is pushed to this thread's
        // stale local_queue and never stolen / woken properly.
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(5));
            let task = pool.spawn(async { 42 });
            assert_eq!(block_on(task), 42);
        }
        let _ = tx.send(());
    });
    rx.recv_timeout(Duration::from_secs(2))
        .expect("deadlocked in bevy_tasks (Bevy #12044)");
}
