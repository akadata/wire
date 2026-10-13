use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
pub trait Runnable {
    fn poll_run(&mut self, cx: &mut Context<'_>) -> Poll<()>;
}
pub struct Task<F> {
    // Stored inline: `Task` itself always lives inside a `Box<dyn Runnable>`
    // whose heap address never changes (the `Box` pointer is moved, never the
    // pointee), so pin-projecting `future` here is sound and saves one heap
    // allocation per `spawn` versus `Pin<Box<F>>`.
    future: F,
}
impl<F> Task<F>
where
    F: Future<Output = ()> + 'static,
{
    pub fn new(future: F) -> Self {
        Self { future }
    }
}
impl<F> Runnable for Task<F>
where
    F: Future<Output = ()> + 'static,
{
    fn poll_run(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        // SAFETY: `Task` always lives inside a `Box<dyn Runnable>` and is only
        // ever moved as a pointer (the pointee address is stable across polls),
        // so the inline future never moves while pinned.
        let pinned = unsafe { Pin::new_unchecked(&mut self.future) };
        pinned.poll(cx)
    }
}
pub struct TaskSlot {
    pub runnable: Option<Box<dyn Runnable>>,
    pub is_queued: bool,
    pub is_polling: bool,
    pub woke_while_polling: bool,
    pub generation: u32,
}
impl TaskSlot {
    pub const fn empty() -> Self {
        Self {
            runnable: None,
            is_queued: false,
            is_polling: false,
            woke_while_polling: false,
            generation: 0,
        }
    }
}
