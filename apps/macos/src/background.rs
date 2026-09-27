//! Running slow work off the main thread.
//!
//! Decryption, encryption and file I/O run on a Grand Central Dispatch
//! background queue so the UI never freezes. Results come back to the main
//! queue, where all AppKit calls and all `DocumentSession` mutations happen.
//! Completion closures must be `Send`, so they capture only plain data (such
//! as a `DocumentId`) and look the window up again on the main thread.

use dispatch2::{DispatchQueue, DispatchQueueGlobalPriority, GlobalQueueIdentifier};

/// Runs `work` on a background queue, then `then(result)` on the main queue.
pub fn run_in_background<T, W, C>(work: W, then: C)
where
    T: Send + 'static,
    W: FnOnce() -> T + Send + 'static,
    C: FnOnce(T) + Send + 'static,
{
    let queue = DispatchQueue::global_queue(GlobalQueueIdentifier::Priority(
        DispatchQueueGlobalPriority::High,
    ));
    queue.exec_async(move || {
        let result = work();
        DispatchQueue::main().exec_async(move || then(result));
    });
}

/// Runs `work` on the main queue at the next opportunity.
pub fn on_main_queue(work: impl FnOnce() + Send + 'static) {
    DispatchQueue::main().exec_async(work);
}
