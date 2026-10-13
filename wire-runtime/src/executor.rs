use std::cell::RefCell;
use std::collections::VecDeque;
use std::os::unix::io::AsRawFd;
use std::task::{Context, Poll};
use io_uring::IoUring;
use crate::task::{Runnable, Task, TaskSlot};
use crate::waker::create_waker;
pub const MAX_TASKS: usize = 4096;
thread_local! {
    static CURRENT_EXECUTOR: RefCell<Option<*mut LocalExecutor>> = RefCell::new(None);
}
pub fn wake_task_by_id(task_id: usize) {
    CURRENT_EXECUTOR.with(|exec_cell| {
        if let Some(exec_ptr) = *exec_cell.borrow() {
            // SAFETY: Dereferencing current thread-local pinned executor pointer.
            unsafe {
                (*exec_ptr).enqueue_task(task_id);
            }
        }
    });
}
pub struct LocalExecutor {
    core_id: usize,
    slots: Vec<TaskSlot>,
    run_queue: VecDeque<usize>,
    free_slots: Vec<usize>,
    ring: IoUring,
}
impl LocalExecutor {
    pub fn new(core_id: usize) -> anyhow::Result<Self> {
        let _ = wire_xdp::pin_thread_to_core(core_id);
        let mut slots = Vec::with_capacity(MAX_TASKS);
        let mut free_slots = Vec::with_capacity(MAX_TASKS);
        for i in 0..MAX_TASKS {
            slots.push(TaskSlot::empty());
            free_slots.push(MAX_TASKS - 1 - i);
        }
        let ring = match IoUring::builder().setup_sqpoll(2000).build(512) {
            Ok(r) => r,
            Err(_) => IoUring::new(512)?,
        };
        Ok(Self {
            core_id,
            slots,
            run_queue: VecDeque::with_capacity(MAX_TASKS),
            free_slots,
            ring,
        })
    }
    pub fn core_id(&self) -> usize {
        self.core_id
    }
    pub fn ring_fd(&self) -> std::os::unix::io::RawFd {
        self.ring.as_raw_fd()
    }
    pub fn spawn<F>(&mut self, future: F) -> Option<usize>
    where
        F: std::future::Future<Output = ()> + 'static,
    {
        let slot_idx = self.free_slots.pop()?;
        let slot = &mut self.slots[slot_idx];
        slot.runnable = Some(Box::new(Task::new(future)));
        slot.is_queued = true;
        slot.generation = slot.generation.wrapping_add(1);
        self.run_queue.push_back(slot_idx);
        Some(slot_idx)
    }
    #[inline(always)]
    pub fn enqueue_task(&mut self, task_id: usize) {
        if task_id < MAX_TASKS {
            let slot = &mut self.slots[task_id];
            if slot.is_polling {
                // Woken from inside its own `poll`: the runnable is taken out
                // right now, so record it and re-queue after `poll` returns.
                slot.woke_while_polling = true;
            } else if slot.runnable.is_some() && !slot.is_queued {
                slot.is_queued = true;
                self.run_queue.push_back(task_id);
            }
        }
    }
    /// Poll one taken runnable to completion-or-pending.
    /// Returns true when the task reached `Ready`.
    #[inline(always)]
    fn poll_task_slot(&mut self, task_id: usize, mut runnable: Box<dyn Runnable>) -> bool {
        self.slots[task_id].is_polling = true;
        let waker = create_waker(task_id);
        let mut cx = Context::from_waker(&waker);
        let completed = match runnable.poll_run(&mut cx) {
            Poll::Ready(()) => true,
            Poll::Pending => false,
        };
        let slot = &mut self.slots[task_id];
        slot.is_polling = false;
        if completed {
            slot.runnable = None;
            slot.is_queued = false;
            slot.woke_while_polling = false;
            self.free_slots.push(task_id);
            true
        } else if slot.woke_while_polling {
            // Woken from inside its own `poll`: re-queue instead of parking.
            slot.woke_while_polling = false;
            slot.runnable = Some(runnable);
            slot.is_queued = true;
            self.run_queue.push_back(task_id);
            false
        } else {
            slot.runnable = Some(runnable);
            false
        }
    }
    pub fn run_single_tick(&mut self) -> usize {
        let prev = CURRENT_EXECUTOR.with(|c| c.replace(Some(self as *mut _)));
        let mut completed_count = 0;
        let mut budget = 64;
        while budget > 0 {
            let task_id = match self.run_queue.pop_front() {
                Some(id) => id,
                None => break,
            };
            budget -= 1;
            let runnable = {
                let slot = &mut self.slots[task_id];
                slot.is_queued = false;
                slot.runnable.take()
            };
            if let Some(task) = runnable {
                if self.poll_task_slot(task_id, task) {
                    completed_count += 1;
                }
            }
        }
        let _ = self.ring.submit();
        CURRENT_EXECUTOR.with(|c| c.replace(prev));
        completed_count
    }
    /// Drain the entire run queue in one guarded section.
    ///
    /// Same per-task semantics as repeated `run_single_tick()` calls, but with
    /// a single thread-local guard and a single `ring.submit()` for the whole
    /// drain instead of one of each per 64-task tick. Returns the number of
    /// tasks that reached `Ready`.
    pub fn drain_all(&mut self) -> usize {
        let prev = CURRENT_EXECUTOR.with(|c| c.replace(Some(self as *mut _)));
        let mut completed_count = 0;
        loop {
            let task_id = match self.run_queue.pop_front() {
                Some(id) => id,
                None => break,
            };
            let runnable = {
                let slot = &mut self.slots[task_id];
                slot.is_queued = false;
                slot.runnable.take()
            };
            if let Some(task) = runnable {
                if self.poll_task_slot(task_id, task) {
                    completed_count += 1;
                }
            }
        }
        // One submit flushes anything queued on the ring during the drain.
        // (This executor never reaps CQEs in the poll path, so collapsing the
        // per-tick submits changes no observable polling behaviour.)
        let _ = self.ring.submit();
        CURRENT_EXECUTOR.with(|c| c.replace(prev));
        completed_count
    }
    pub fn run_until_stalled(&mut self) {
        // Fast path: one guard + one submit for the whole drain.
        let _ = self.drain_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::Poll;

    struct YieldOnce {
        yielded: bool,
    }
    impl std::future::Future for YieldOnce {
        type Output = ();
        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<()> {
            if self.yielded {
                Poll::Ready(())
            } else {
                self.yielded = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    #[test]
    fn ready_tasks_drain_with_exact_counts() {
        let mut ex = LocalExecutor::new(0).unwrap();
        let mut accepted = 0;
        for _ in 0..1024 {
            assert!(ex.spawn(async {}).is_some());
            accepted += 1;
        }
        assert_eq!(accepted, 1024);
        assert_eq!(ex.drain_all(), 1024);
        assert!(ex.run_queue.is_empty());
    }

    #[test]
    fn pending_then_wake_completes() {
        // Exercises the Pending store-back path (inline-future pinning).
        let mut ex = LocalExecutor::new(0).unwrap();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        assert!(ex
            .spawn(async move {
                YieldOnce { yielded: false }.await;
                c.fetch_add(1, Ordering::SeqCst);
            })
            .is_some());
        // First drain: pends after re-queueing itself via wake.
        // Second drain: completes.
        ex.run_until_stalled();
        ex.run_until_stalled();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert!(ex.run_queue.is_empty());
    }

    #[test]
    fn spawn_beyond_capacity_is_rejected() {
        let mut ex = LocalExecutor::new(0).unwrap();
        let mut accepted = 0;
        for _ in 0..(MAX_TASKS + 100) {
            if ex.spawn(async {}).is_some() {
                accepted += 1;
            }
        }
        assert_eq!(accepted, MAX_TASKS);
        assert_eq!(ex.drain_all(), MAX_TASKS);
    }
}
