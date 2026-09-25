//! Caller-supplied transfer profile for static acquisition, validated
//! against crate ceilings.
//!
//! No numeric limit has an invented default: the caller states the redirect
//! budget, both timeouts, and the wire body cap. The decoded-body, text,
//! URL, and header-section caps default to their ceilings, which come only
//! from landed or external sources (see each `*_CEILING` constant).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use snafu::ensure;

use crate::error::{Error, InvalidConstraintSnafu, Result};

/// Which URL schemes an acquisition may fetch. Applies to the requested URL
/// and to every redirect target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum SchemePolicy {
    /// Only `https`.
    HttpsOnly,
    /// `http` and `https`. An `https` to `http` redirect is still governed
    /// by [`DowngradePolicy`].
    HttpAndHttps,
}

impl SchemePolicy {
    /// Whether `scheme` (already lowercase, as `url::Url` stores it) is in
    /// this set.
    pub(crate) fn permits(self, scheme: &str) -> bool {
        match self {
            Self::HttpsOnly => scheme == "https",
            Self::HttpAndHttps => scheme == "https" || scheme == "http",
        }
    }
}

/// Whether a redirect from an `https` hop to an `http` target may be
/// followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum DowngradePolicy {
    /// Refuse the downgrade before any socket is opened for the `http`
    /// target. The default.
    #[default]
    Refuse,
    /// Follow the downgrade. The caller accepts that the rest of the chain
    /// travels without transport security.
    Allow,
}

/// The exact transfer profile a [`super::StaticAcquirer`] applies.
///
/// Fields are private: every constructor, builder step, and
/// deserialization passes through the same ceiling validation, so a value
/// of this type is always within the crate ceilings.
///
/// ```compile_fail
/// # use sylloge::AcquisitionLimits;
/// fn widen(limits: &mut AcquisitionLimits) {
///     limits.max_redirects = 1_000;
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "LimitsWire", into = "LimitsWire")]
pub struct AcquisitionLimits {
    schemes: SchemePolicy,
    downgrade: DowngradePolicy,
    max_redirects: u32,
    connect_timeout: Duration,
    deadline: Duration,
    max_body_bytes: u64,
    max_decoded_bytes: u64,
    max_text_bytes: u64,
    max_url_bytes: usize,
    max_header_bytes: usize,
}

impl AcquisitionLimits {
    /// Redirect ceiling: the WHATWG Fetch standard's redirect limit (HTTP
    /// redirect fetch: "If request's redirect count is 20, then return a
    /// network error").
    pub const MAX_REDIRECTS_CEILING: u32 = 20;

    /// Wire body ceiling in bytes (10 MiB), carried over from the retired
    /// `PageContent::MAX_BODY_BYTES`.
    pub const MAX_BODY_BYTES_CEILING: u64 = 10 * 1024 * 1024;

    /// Decoded body ceiling in bytes (10 MiB): the same retired
    /// `PageContent::MAX_BODY_BYTES`, which bounded the body a consumer
    /// held. Applies after the content coding is removed, so a small
    /// compressed body cannot expand past it.
    pub const MAX_DECODED_BYTES_CEILING: u64 = 10 * 1024 * 1024;

    /// Extracted-text ceiling in bytes (4 MiB), carried over from the
    /// retired `PageContent::MAX_TEXT_BYTES`.
    pub const MAX_TEXT_BYTES_CEILING: u64 = 4 * 1024 * 1024;

    /// URL ceiling in bytes (8 KiB), carried over from the retired
    /// `PageContent::MAX_URL_BYTES`. Applies to the requested URL and to
    /// every resolved redirect target.
    pub const MAX_URL_BYTES_CEILING: usize = 8 * 1024;

    /// Response header-section ceiling in bytes: hyper's default HTTP/1
    /// `max_buf_size` (`8192 + 4096 * 100`), the buffer the head must fit
    /// in.
    pub const MAX_HEADER_BYTES_CEILING: usize = 8192 + 4096 * 100;

    /// Response header-section floor in bytes: hyper rejects (panics on) a
    /// `max_buf_size` below its initial read buffer of 8192 bytes.
    pub const MIN_HEADER_BYTES: usize = 8192;

    /// Build a profile. `max_decoded_bytes`, `max_text_bytes`,
    /// `max_url_bytes`, and `max_header_bytes` start at their ceilings and
    /// [`DowngradePolicy::Refuse`] applies; narrow them with the `with_*`
    /// builders. `max_body_bytes` bounds the body as received on the wire.
    ///
    /// `connect_timeout` bounds each connection attempt; `deadline` bounds
    /// the whole operation (every resolution, connection, TLS handshake,
    /// request, and body read across every hop).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConstraint`] when `max_redirects` exceeds
    /// [`Self::MAX_REDIRECTS_CEILING`], `max_body_bytes` exceeds
    /// [`Self::MAX_BODY_BYTES_CEILING`], or either duration is zero, not a
    /// whole number of milliseconds, or not representable as `u64`
    /// milliseconds.
    pub fn new(
        schemes: SchemePolicy,
        max_redirects: u32,
        connect_timeout: Duration,
        deadline: Duration,
        max_body_bytes: u64,
    ) -> Result<Self> {
        Self {
            schemes,
            downgrade: DowngradePolicy::Refuse,
            max_redirects,
            connect_timeout,
            deadline,
            max_body_bytes,
            max_decoded_bytes: Self::MAX_DECODED_BYTES_CEILING,
            max_text_bytes: Self::MAX_TEXT_BYTES_CEILING,
            max_url_bytes: Self::MAX_URL_BYTES_CEILING,
            max_header_bytes: Self::MAX_HEADER_BYTES_CEILING,
        }
        .validated()
    }

    /// Builder: set the downgrade policy.
    #[must_use]
    pub fn with_downgrade(mut self, downgrade: DowngradePolicy) -> Self {
        self.downgrade = downgrade;
        self
    }

    /// Builder: narrow the decoded body cap.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConstraint`] when `max_decoded_bytes`
    /// exceeds [`Self::MAX_DECODED_BYTES_CEILING`].
    pub fn with_max_decoded_bytes(mut self, max_decoded_bytes: u64) -> Result<Self> {
        self.max_decoded_bytes = max_decoded_bytes;
        self.validated()
    }

    /// Builder: narrow the extracted-text cap.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConstraint`] when `max_text_bytes` exceeds
    /// [`Self::MAX_TEXT_BYTES_CEILING`].
    pub fn with_max_text_bytes(mut self, max_text_bytes: u64) -> Result<Self> {
        self.max_text_bytes = max_text_bytes;
        self.validated()
    }

    /// Builder: narrow the URL cap.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConstraint`] when `max_url_bytes` is zero or
    /// exceeds [`Self::MAX_URL_BYTES_CEILING`].
    pub fn with_max_url_bytes(mut self, max_url_bytes: usize) -> Result<Self> {
        self.max_url_bytes = max_url_bytes;
        self.validated()
    }

    /// Builder: narrow the response header-section cap.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConstraint`] when `max_header_bytes` is outside
    /// [`Self::MIN_HEADER_BYTES`]`..=`[`Self::MAX_HEADER_BYTES_CEILING`].
    pub fn with_max_header_bytes(mut self, max_header_bytes: usize) -> Result<Self> {
        self.max_header_bytes = max_header_bytes;
        self.validated()
    }

    /// Schemes permitted on every hop.
    #[must_use]
    pub const fn schemes(&self) -> SchemePolicy {
        self.schemes
    }

    /// Whether an `https` to `http` redirect may be followed.
    #[must_use]
    pub const fn downgrade(&self) -> DowngradePolicy {
        self.downgrade
    }

    /// Maximum number of redirects followed after the requested URL.
    #[must_use]
    pub const fn max_redirects(&self) -> u32 {
        self.max_redirects
    }

    /// Bound on each connection attempt.
    #[must_use]
    pub const fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }

    /// Bound on the whole operation.
    #[must_use]
    pub const fn deadline(&self) -> Duration {
        self.deadline
    }

    /// Maximum accepted response body, in bytes on the wire.
    #[must_use]
    pub const fn max_body_bytes(&self) -> u64 {
        self.max_body_bytes
    }

    /// Maximum accepted body after its content coding is removed.
    #[must_use]
    pub const fn max_decoded_bytes(&self) -> u64 {
        self.max_decoded_bytes
    }

    /// Maximum extracted text, in UTF-8 bytes.
    #[must_use]
    pub const fn max_text_bytes(&self) -> u64 {
        self.max_text_bytes
    }

    /// Maximum accepted URL length, in bytes.
    #[must_use]
    pub const fn max_url_bytes(&self) -> usize {
        self.max_url_bytes
    }

    /// Maximum accepted response header section, in bytes.
    #[must_use]
    pub const fn max_header_bytes(&self) -> usize {
        self.max_header_bytes
    }

    /// [`Self::connect_timeout`] in whole milliseconds, for evidence.
    pub(crate) fn connect_timeout_ms(&self) -> u64 {
        saturating_millis(self.connect_timeout)
    }

    /// [`Self::deadline`] in whole milliseconds, for evidence.
    pub(crate) fn deadline_ms(&self) -> u64 {
        saturating_millis(self.deadline)
    }

    fn validated(self) -> Result<Self> {
        ensure!(
            self.max_redirects <= Self::MAX_REDIRECTS_CEILING,
            InvalidConstraintSnafu {
                field: "max_redirects",
                reason: format!(
                    "max_redirects {} exceeds the ceiling {}",
                    self.max_redirects,
                    Self::MAX_REDIRECTS_CEILING
                ),
            }
        );
        ensure!(
            self.max_body_bytes <= Self::MAX_BODY_BYTES_CEILING,
            InvalidConstraintSnafu {
                field: "max_body_bytes",
                reason: format!(
                    "max_body_bytes {} exceeds the ceiling {}",
                    self.max_body_bytes,
                    Self::MAX_BODY_BYTES_CEILING
                ),
            }
        );
        ensure!(
            self.max_decoded_bytes <= Self::MAX_DECODED_BYTES_CEILING,
            InvalidConstraintSnafu {
                field: "max_decoded_bytes",
                reason: format!(
                    "max_decoded_bytes {} exceeds the ceiling {}",
                    self.max_decoded_bytes,
                    Self::MAX_DECODED_BYTES_CEILING
                ),
            }
        );
        ensure!(
            self.max_text_bytes <= Self::MAX_TEXT_BYTES_CEILING,
            InvalidConstraintSnafu {
                field: "max_text_bytes",
                reason: format!(
                    "max_text_bytes {} exceeds the ceiling {}",
                    self.max_text_bytes,
                    Self::MAX_TEXT_BYTES_CEILING
                ),
            }
        );
        ensure!(
            (1..=Self::MAX_URL_BYTES_CEILING).contains(&self.max_url_bytes),
            InvalidConstraintSnafu {
                field: "max_url_bytes",
                reason: format!(
                    "max_url_bytes {} is outside 1..={}",
                    self.max_url_bytes,
                    Self::MAX_URL_BYTES_CEILING
                ),
            }
        );
        ensure!(
            (Self::MIN_HEADER_BYTES..=Self::MAX_HEADER_BYTES_CEILING)
                .contains(&self.max_header_bytes),
            InvalidConstraintSnafu {
                field: "max_header_bytes",
                reason: format!(
                    "max_header_bytes {} is outside {}..={}",
                    self.max_header_bytes,
                    Self::MIN_HEADER_BYTES,
                    Self::MAX_HEADER_BYTES_CEILING
                ),
            }
        );
        whole_millis("connect_timeout", self.connect_timeout)?;
        whole_millis("deadline", self.deadline)?;
        Ok(self)
    }
}

/// Validate that `duration` is non-zero and a whole number of milliseconds
/// representable as `u64`, returning that count.
///
/// WHY: the serialized profile records durations in milliseconds; refusing
/// sub-millisecond remainders keeps a serialized profile identical to the
/// one applied instead of silently rounding it.
fn whole_millis(name: &str, duration: Duration) -> Result<u64> {
    let millis = u64::try_from(duration.as_millis()).map_err(|_| {
        InvalidConstraintSnafu {
            field: name,
            reason: "does not fit in u64 milliseconds",
        }
        .build()
    })?;
    ensure!(
        millis > 0 && Duration::from_millis(millis) == duration,
        InvalidConstraintSnafu {
            field: name,
            reason: "must be a non-zero whole number of milliseconds",
        }
    );
    Ok(millis)
}

/// `duration` in whole milliseconds, saturating at `u64::MAX`.
///
/// NOTE: for an `AcquisitionLimits` duration the saturation is
/// unreachable: `validated` proved both durations are whole milliseconds
/// that fit in `u64`.
pub(crate) fn saturating_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Serialized form of [`AcquisitionLimits`]; durations travel as whole
/// milliseconds.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LimitsWire {
    schemes: SchemePolicy,
    downgrade: DowngradePolicy,
    max_redirects: u32,
    connect_timeout_ms: u64,
    deadline_ms: u64,
    max_body_bytes: u64,
    max_decoded_bytes: u64,
    max_text_bytes: u64,
    max_url_bytes: usize,
    max_header_bytes: usize,
}

impl TryFrom<LimitsWire> for AcquisitionLimits {
    type Error = Error;

    fn try_from(wire: LimitsWire) -> Result<Self> {
        Self {
            schemes: wire.schemes,
            downgrade: wire.downgrade,
            max_redirects: wire.max_redirects,
            connect_timeout: Duration::from_millis(wire.connect_timeout_ms),
            deadline: Duration::from_millis(wire.deadline_ms),
            max_body_bytes: wire.max_body_bytes,
            max_decoded_bytes: wire.max_decoded_bytes,
            max_text_bytes: wire.max_text_bytes,
            max_url_bytes: wire.max_url_bytes,
            max_header_bytes: wire.max_header_bytes,
        }
        .validated()
    }
}

impl From<AcquisitionLimits> for LimitsWire {
    fn from(limits: AcquisitionLimits) -> Self {
        Self {
            schemes: limits.schemes,
            downgrade: limits.downgrade,
            max_redirects: limits.max_redirects,
            connect_timeout_ms: saturating_millis(limits.connect_timeout),
            deadline_ms: saturating_millis(limits.deadline),
            max_body_bytes: limits.max_body_bytes,
            max_decoded_bytes: limits.max_decoded_bytes,
            max_text_bytes: limits.max_text_bytes,
            max_url_bytes: limits.max_url_bytes,
            max_header_bytes: limits.max_header_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(max_redirects: u32) -> Result<AcquisitionLimits> {
        AcquisitionLimits::new(
            SchemePolicy::HttpsOnly,
            max_redirects,
            Duration::from_secs(5),
            Duration::from_secs(30),
            1024,
        )
    }

    #[test]
    fn new_accepts_the_redirect_ceiling() {
        let limits = limits(AcquisitionLimits::MAX_REDIRECTS_CEILING).unwrap();
        assert_eq!(
            limits.max_redirects(),
            20,
            "the Fetch redirect limit itself is a valid profile"
        );
        assert_eq!(
            limits.downgrade(),
            DowngradePolicy::Refuse,
            "downgrade must be refused unless the caller opts in"
        );
    }

    #[test]
    fn new_rejects_redirects_above_ceiling() {
        let err = limits(AcquisitionLimits::MAX_REDIRECTS_CEILING + 1).unwrap_err();
        assert!(
            matches!(err, Error::InvalidConstraint { .. }),
            "over-ceiling redirects must be an invalid constraint: {err:?}"
        );
    }

    #[test]
    fn new_rejects_body_above_ceiling() {
        let err = AcquisitionLimits::new(
            SchemePolicy::HttpsOnly,
            0,
            Duration::from_secs(1),
            Duration::from_secs(1),
            AcquisitionLimits::MAX_BODY_BYTES_CEILING + 1,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("max_body_bytes"),
            "error must name the rejected field: {err}"
        );
    }

    #[test]
    fn new_rejects_zero_and_fractional_durations() {
        for (connect, deadline) in [
            (Duration::ZERO, Duration::from_secs(1)),
            (Duration::from_secs(1), Duration::ZERO),
            (Duration::from_micros(1_500), Duration::from_secs(1)),
            (Duration::from_secs(1), Duration::MAX),
        ] {
            let result = AcquisitionLimits::new(SchemePolicy::HttpsOnly, 0, connect, deadline, 0);
            assert!(
                result.is_err(),
                "({connect:?}, {deadline:?}) must be rejected"
            );
        }
    }

    #[test]
    fn header_bytes_must_stay_within_hyper_bounds() {
        let base = limits(0).unwrap();
        assert!(
            base.clone()
                .with_max_header_bytes(AcquisitionLimits::MIN_HEADER_BYTES - 1)
                .is_err(),
            "below hyper's minimum buffer must be rejected, not panic later"
        );
        assert!(
            base.clone()
                .with_max_header_bytes(AcquisitionLimits::MAX_HEADER_BYTES_CEILING + 1)
                .is_err(),
            "above the ceiling must be rejected"
        );
        assert!(
            base.with_max_header_bytes(AcquisitionLimits::MIN_HEADER_BYTES)
                .is_ok(),
            "the floor itself is valid"
        );
    }

    #[test]
    fn decoded_and_text_caps_default_to_and_stay_within_ceilings() {
        let base = limits(0).unwrap();
        assert_eq!(
            base.max_decoded_bytes(),
            AcquisitionLimits::MAX_DECODED_BYTES_CEILING,
            "decoded cap starts at its ceiling"
        );
        assert_eq!(
            base.max_text_bytes(),
            AcquisitionLimits::MAX_TEXT_BYTES_CEILING,
            "text cap starts at its ceiling"
        );
        let err = base
            .clone()
            .with_max_decoded_bytes(AcquisitionLimits::MAX_DECODED_BYTES_CEILING + 1)
            .unwrap_err();
        assert!(
            err.to_string().contains("max_decoded_bytes"),
            "error must name the rejected field: {err}"
        );
        let err = base
            .clone()
            .with_max_text_bytes(AcquisitionLimits::MAX_TEXT_BYTES_CEILING + 1)
            .unwrap_err();
        assert!(
            err.to_string().contains("max_text_bytes"),
            "error must name the rejected field: {err}"
        );
        let narrowed = base
            .with_max_decoded_bytes(64)
            .and_then(|l| l.with_max_text_bytes(16))
            .unwrap();
        assert_eq!(
            (narrowed.max_decoded_bytes(), narrowed.max_text_bytes()),
            (64, 16),
            "narrowing within the ceilings is kept"
        );
    }

    #[test]
    fn url_bytes_must_stay_within_ceiling() {
        let base = limits(0).unwrap();
        assert!(
            base.clone().with_max_url_bytes(0).is_err(),
            "a zero URL cap admits nothing"
        );
        assert!(
            base.with_max_url_bytes(AcquisitionLimits::MAX_URL_BYTES_CEILING + 1)
                .is_err(),
            "above the ceiling must be rejected"
        );
    }

    #[test]
    fn serde_round_trips_with_snake_case_millis() {
        let limits = limits(3)
            .unwrap()
            .with_downgrade(DowngradePolicy::Allow)
            .with_max_url_bytes(2048)
            .unwrap();
        let json = serde_json::to_value(&limits).unwrap();
        assert_eq!(json["connect_timeout_ms"], 5_000, "timeouts travel as ms");
        assert_eq!(json["schemes"], "https_only", "enums are snake_case");
        assert_eq!(json["downgrade"], "allow", "enums are snake_case");
        let back: AcquisitionLimits = serde_json::from_value(json).unwrap();
        assert_eq!(back, limits, "round trip must be exact");
    }

    #[test]
    fn deserialize_rejects_over_ceiling_profile() {
        let mut json = serde_json::to_value(limits(0).unwrap()).unwrap();
        json["max_redirects"] = serde_json::Value::from(21);
        let err = serde_json::from_value::<AcquisitionLimits>(json).unwrap_err();
        assert!(
            err.to_string().contains("max_redirects"),
            "deserialization must apply the same ceiling: {err}"
        );
    }

    #[test]
    fn scheme_policy_permits_only_its_members() {
        assert!(
            SchemePolicy::HttpsOnly.permits("https"),
            "https in https_only"
        );
        assert!(
            !SchemePolicy::HttpsOnly.permits("http"),
            "http not in https_only"
        );
        assert!(
            SchemePolicy::HttpAndHttps.permits("http"),
            "http in http_and_https"
        );
        assert!(
            !SchemePolicy::HttpAndHttps.permits("ftp"),
            "ftp never permitted"
        );
    }
}
