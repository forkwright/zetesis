//! Crate-internal serde helpers enforcing type invariants at the
//! deserialization boundary.

use serde::{Deserialize, Deserializer};

/// Deserialize an `f32` and clamp it into `0.0..=1.0`, mapping NaN to `0.0`.
///
/// WHY: [`crate::ResultHit::new`] and [`crate::Citation::new`] clamp at
/// construction; without this the serde path would smuggle out-of-range or
/// NaN scores past the documented invariant.
pub(crate) fn clamp_unit_f32<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: Deserializer<'de>,
{
    let value = f32::deserialize(deserializer)?;
    Ok(if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    })
}
