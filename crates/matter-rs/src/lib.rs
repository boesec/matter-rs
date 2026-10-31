//! matter-rs: a Matter bridge and controller in Rust.
//!
//! This crate is a placeholder that gives the quality gate something to build and test. It
//! will be replaced by the real crate layout.
//!
//! Unsafe code is forbidden in every crate of this workspace, twice over: by the workspace lint
//! `unsafe_code = "forbid"` and by the `#![forbid(unsafe_code)]` attribute at the crate root.
//! Doc tests are compiled as separate crates and do not inherit either, so the crate passes the
//! same rule on to them with `#![doc(test(attr(forbid(unsafe_code))))]`. The following block is
//! otherwise valid Rust and must not compile:
//!
//! ```compile_fail
//! let value = 1_u8;
//! let pointer = &raw const value;
//! // SAFETY: `pointer` points to `value`, which is alive and initialised.
//! let copy = unsafe { *pointer };
//! assert_eq!(copy, 1);
//! ```

#![forbid(unsafe_code)]
#![doc(test(attr(forbid(unsafe_code))))]

/// Name of the project, as used in user-visible output.
pub const PROJECT_NAME: &str = "matter-rs";

#[cfg(test)]
mod tests {
    use super::PROJECT_NAME;

    #[test]
    fn project_name_is_matter_rs() {
        assert_eq!(PROJECT_NAME, "matter-rs");
    }
}
