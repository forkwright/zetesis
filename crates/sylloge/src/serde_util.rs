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

/// `skip_serializing_if` predicate for a count whose absence means zero.
///
/// WHY: a defaulted count added to an existing record keeps that record's
/// serialized form unchanged whenever the count is zero.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes the field by reference"
)]
pub(crate) fn is_zero(count: &usize) -> bool {
    *count == 0
}

/// Serialize a value as its `Display` text and parse it back with
/// `FromStr`, in every serde format.
///
/// WHY: `std::net` addresses serialize as text only in human-readable
/// formats and as structured values in binary ones (CBOR), so without this
/// the same evidence record would have two shapes. Evidence carries one
/// shape in JSON and CBOR alike.
pub(crate) mod as_text {
    use std::fmt::Display;
    use std::str::FromStr;

    use serde::{Deserialize, Deserializer, Serializer, de};

    pub(crate) fn serialize<T, S>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
    where
        T: Display,
        S: Serializer,
    {
        serializer.collect_str(value)
    }

    pub(crate) fn deserialize<'de, T, D>(deserializer: D) -> Result<T, D::Error>
    where
        T: FromStr,
        T::Err: Display,
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

/// [`as_text`] for each element of a list.
pub(crate) mod list_as_text {
    use std::fmt::Display;
    use std::str::FromStr;

    use serde::{Deserialize, Deserializer, Serializer, de};

    pub(crate) fn serialize<T, S>(values: &[T], serializer: S) -> Result<S::Ok, S::Error>
    where
        T: Display,
        S: Serializer,
    {
        serializer.collect_seq(values.iter().map(ToString::to_string))
    }

    pub(crate) fn deserialize<'de, T, D>(deserializer: D) -> Result<Vec<T>, D::Error>
    where
        T: FromStr,
        T::Err: Display,
        D: Deserializer<'de>,
    {
        Vec::<String>::deserialize(deserializer)?
            .iter()
            .map(|text| text.parse().map_err(de::Error::custom))
            .collect()
    }
}
