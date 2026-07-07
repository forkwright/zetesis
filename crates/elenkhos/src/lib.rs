#![doc = "Reserved crate boundary for the zetesis retrospective steel-manning engine."]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

/// Marker type for the locked `elenkhos` crate boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elenkhos;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_type_upholds_value_semantics() {
        // WHY: the marker locks the crate boundary; its derives (Copy,
        // Eq, Debug) are the public contract downstream facades re-export.
        let a = Elenkhos;
        let b = a;
        assert_eq!(a, b);
        assert_eq!(format!("{a:?}"), "Elenkhos");
    }
}
