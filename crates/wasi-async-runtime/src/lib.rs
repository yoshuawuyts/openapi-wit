//! A single-threaded native runtime for WASI 0.2
//!
//! The way to use this is to call [`block_on()`] to obtain an instance of
//! [`Reactor`]. You can then share the reactor in code that needs it to insert
//! instances of
//! [`wasi::Pollable`](https://docs.rs/wasi/latest/wasi/io/poll/struct.Pollable.html).
//! This will automatically wait for the futures to resolve, and call the
//! necessary wakers to work.

#![forbid(rust_2018_idioms)]
#![deny(missing_debug_implementations, nonstandard_style)]
#![warn(missing_docs, future_incompatible, unreachable_pub)]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod block_on;
mod polling;
mod reactor;

pub use block_on::block_on;
pub use reactor::{PollHandle, Reactor};
