//! Engine implementation of matter-rs on rs-matter (layer 1).
//!
//! This is the only crate of the workspace that may depend on rs-matter; a workspace test
//! enforces that. It implements the engine facade over rs-matter and hides everything about it:
//! rs-matter runs on a dedicated OS thread owned by this crate, with a single-threaded executor,
//! and the rest of the product talks to that thread through channels behind the facade's
//! asynchronous API. The thread, its executor and its channels are implementation details.
//!
//! The crate needs the standard library (threads, sockets, files). A microcontroller engine is a
//! separate engine implementation crate, not a feature of this one.
//!
//! The rs-matter dependency itself is not pinned yet; until it is, this crate holds only its
//! error type.

#![forbid(unsafe_code)]
#![doc(test(attr(forbid(unsafe_code))))]

mod error;

pub use error::Error;
