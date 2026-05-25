#![doc = "Zetesis facade crate."]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

/// Bootstrap marker for the pre-implementation workspace.
///
/// The first Phase 1 implementation pass should replace this with the public
/// facade API once the crate boundaries are ready.
pub const BOOTSTRAP_MARKER: &str = "zetesis-phase-0-bootstrap";

#[cfg(test)]
mod tests {
    use super::BOOTSTRAP_MARKER;

    #[test]
    fn exposes_bootstrap_marker() {
        assert_eq!(BOOTSTRAP_MARKER, "zetesis-phase-0-bootstrap");
    }
}
