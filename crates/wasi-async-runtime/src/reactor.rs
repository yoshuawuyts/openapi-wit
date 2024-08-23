use super::{
    block_on::noop_waker,
    polling::{EventKey, Poller},
};

use alloc::rc::Rc;
use core::cell::RefCell;
use core::task::Poll;
use core::task::Waker;
use core::{future, task};
#[cfg(not(feature = "std"))]
use hashbrown::HashMap;
#[cfg(feature = "std")]
use std::collections::HashMap;
use wasi::io::poll::Pollable;

/// Manage async system resources for WASI 0.2
#[derive(Debug, Clone)]
pub struct Reactor {
    inner: Rc<RefCell<InnerReactor>>,
}

/// The private, internal `Reactor` implementation - factored out so we can take
/// a lock of the whole.
#[derive(Debug)]
struct InnerReactor {
    poller: Poller,
    wakers: HashMap<EventKey, Waker>,
}

impl Reactor {
    /// Create a new instance of `Reactor`
    pub(crate) fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(InnerReactor {
                poller: Poller::new(),
                wakers: HashMap::new(),
            })),
        }
    }

    /// Block until new events are ready. Calls the respective wakers once done.
    ///
    /// # On Wakers and single-threaded runtimes
    ///
    /// At first glance it might seem silly that this goes through the motions
    /// of calling the wakers. The main waker we create here is a `noop` waker:
    /// it does nothing. However, it is common and encouraged to use wakers to
    /// distinguish between events. Concurrency primitives may construct their
    /// own wakers to keep track of identity and wake more precisely. We do not
    /// control the wakers construted by other libraries, and it is for this
    /// reason that we have to call all the wakers - even if by default they
    /// will do nothing.
    pub(crate) fn block_until(&self) {
        let mut reactor = self.inner.borrow_mut();
        for key in reactor.poller.block_until() {
            match reactor.wakers.get(&key) {
                Some(waker) => waker.wake_by_ref(),
                None => panic!("tried to wake the waker for non-existent `{key:?}`"),
            }
        }
    }

    /// Register interest in a [Pollable].
    ///
    /// If you're not implementing a [Poll] trait, then see [Self::wait_for].
    pub fn register(&self, pollable: Pollable) -> PollHandle {
        PollHandle::register(self.inner.clone(), pollable)
    }

    /// Wait for the pollable to resolve.
    pub async fn wait_for(&self, pollable: Pollable) {
        let mut pollable = Some(pollable);
        let mut handle = None;

        // This function is the core loop of our function; it will be called
        // multiple times as the future is resolving.
        future::poll_fn(|cx| {
            // Reuse the existing [PollHandle] from previous iterations.
            let handle = &*handle.get_or_insert_with(|| {
                // Exchange the [Pollable] for a [PollHandle] on the first iteration.
                self.register(pollable.take().unwrap())
            });

            // Check whether we're ready or need to keep waiting.
            // NOTE: this will be cleaned up on drop.
            handle.poll(cx)
        })
        .await;
    }
}

/// Manages lifecycle of a [Pollable] being registered in a [Reactor]
#[derive(Debug)]
pub struct PollHandle {
    inner: Rc<RefCell<InnerReactor>>,
    key: EventKey,
}

impl PollHandle {
    /// Register interest in a [Pollable].
    ///
    /// If you're not implementing a [Poll] trait, then see [Reactor::wait_for].
    ///
    /// After registration you may then use [Self::poll_for] to both update the
    /// [Waker] and check the [Poll] status of the [Pollable].
    fn register(reactor: Rc<RefCell<InnerReactor>>, pollable: Pollable) -> Self {
        let key = {
            // Take a lock on the reactor. This is single-threaded and
            // short-lived, so it will never be contended.
            let mut reactor = reactor.borrow_mut();

            // Schedule interest in the [Pollable] and register the [Waker] with
            // the [Reactor].
            let key = reactor.poller.insert(pollable);
            reactor.wakers.insert(key, noop_waker());
            key
        };

        Self {
            inner: reactor,
            key,
        }
    }

    /// Check progress of the [Pollable] we're tracking.
    pub fn poll(&self, cx: &mut task::Context<'_>) -> Poll<()> {
        let waker = cx.waker().clone();
        let key = &self.key;
        let mut reactor = self.inner.borrow_mut();

        // Update the [Waker] being tracked.
        reactor.wakers.insert(*key, waker);

        // Check whether we're ready or need to keep waiting.
        if reactor.poller.get(key).unwrap().ready() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl Drop for PollHandle {
    fn drop(&mut self) {
        let key = self.key;
        let mut reactor = self.inner.borrow_mut();
        reactor.poller.remove(key);
        reactor.wakers.remove(&key);
    }
}
