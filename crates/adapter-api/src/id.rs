//! The identity of an adapter instance.

use alloc::string::String;
use core::fmt;

/// The identity of one adapter instance: its kind and the instance name the configuration gave
/// it.
///
/// In the configuration every instance is a table `[adapters.<instance>]` whose `kind` selects
/// the adapter; `kind` is the constant the adapter's factory declares and `instance` the table
/// name. The id appears in log lines and health reports, so it carries no secret.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AdapterId {
    /// The adapter kind, as the configuration spells it (`kind = "mqtt"`).
    pub kind: &'static str,
    /// The instance name, the `<instance>` of `[adapters.<instance>]`.
    pub instance: String,
}

impl AdapterId {
    /// An id of the given kind and instance.
    #[must_use]
    pub fn new(kind: &'static str, instance: impl Into<String>) -> Self {
        Self {
            kind,
            instance: instance.into(),
        }
    }
}

impl fmt::Display for AdapterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.kind, self.instance)
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::AdapterId;

    #[test]
    fn id_displays_kind_and_instance() {
        let id = AdapterId::new("mqtt", "home");
        assert_eq!(id.to_string(), "mqtt/home");
        assert_eq!(id.kind, "mqtt");
        assert_eq!(id.instance, "home");
    }

    #[test]
    fn ids_compare_by_kind_and_instance() {
        assert_eq!(AdapterId::new("mqtt", "a"), AdapterId::new("mqtt", "a"));
        assert_ne!(AdapterId::new("mqtt", "a"), AdapterId::new("mqtt", "b"));
        assert_ne!(AdapterId::new("mqtt", "a"), AdapterId::new("mock", "a"));
    }
}
