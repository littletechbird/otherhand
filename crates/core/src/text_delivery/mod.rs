//! Verified typing: text goes in as small chunks, and each chunk has to be
//! read back from the target before the next one leaves.
//!
//! Sending a key is not the same as the key landing. A Chromium window
//! accepts a key message as soon as its browser thread has handed it to the
//! page, and the page applies it later, to whatever element holds focus
//! when it gets there. A busy or background page can apply it minutes later.
//! So success here is never "every key was sent". It is "the target's own
//! text now holds what was typed", read back after every chunk, with focus
//! checked on the target before every chunk and while each one settles.
//!
//! The platform supplies a [`ChunkTarget`]; this module owns the chunk plan,
//! the comparison, the waits and the failure shapes, so every adapter gets
//! the same rules and the rules are testable without a desktop.

mod chunks;
mod compare;
mod engine;
mod paged;

pub use chunks::{DEFAULT_MAX_CHUNK_CHARS, plan_chunks};
pub use compare::{landed, normalize_for_compare};
pub use engine::{
    ChunkPolicy, ChunkTarget, ChunkedTypeReport, TYPE_NOT_VERIFIED_KIND, type_in_verified_chunks,
};
pub use paged::{DEFAULT_PAGE_CHARS, MAX_READBACK_CHARS, Page, read_paged};

#[cfg(test)]
mod chunks_tests;
#[cfg(test)]
mod compare_tests;
#[cfg(test)]
mod engine_tests;
#[cfg(test)]
mod paged_tests;
