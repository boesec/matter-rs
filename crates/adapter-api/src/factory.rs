//! The factory: from a configuration section to an adapter instance.

use alloc::boxed::Box;

use crate::adapter::Adapter;
use crate::error::Error;
use crate::id::AdapterId;

/// Builds adapter instances of one kind from their configuration.
///
/// Every adapter crate implements this once. It is the piece the configuration is wired to: the
/// config crate composes one enum of adapter sections, tagged by `kind`, with one variant per
/// adapter compiled in, and that variant carries this factory's [`Config`](Self::Config). The
/// derives the config type needs are serde's `Deserialize` (the TOML section) and schemars'
/// `JsonSchema` (the generated schema and reference); fields that are secrets are wrapped so that
/// they never print.
///
/// The service calls [`build`](Self::build) for every `[adapters.<instance>]` table with this
/// kind, once at start and again before every restart of a failed instance, so the factory must
/// be cheap and side-effect free: open no connection here, do that in
/// [`Adapter::run`](crate::Adapter::run).
pub trait AdapterFactory {
    /// The value of `kind` that selects this adapter in the configuration; kebab-case.
    const KIND: &'static str;

    /// The configuration of one instance: the keys of its `[adapters.<instance>]` table besides
    /// `kind`.
    type Config: serde::de::DeserializeOwned + schemars::JsonSchema + Send;

    /// Builds an instance.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the configuration is not usable although it parsed (a consistency
    /// rule the schema cannot express); the supervisor does not retry such an instance.
    fn build(id: AdapterId, config: Self::Config) -> Result<Box<dyn Adapter>, Error>;
}
