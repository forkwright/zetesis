#![doc = "Facade crate for the zetesis sovereign research substrate."]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub use elenkhos as steelman;
pub use sylloge::{
    BudgetConstraint, BudgetExceededSnafu, Citation, CostTracking, Crawler, DeepDepth,
    DeepResearch, Error, ErrorClass, FatalCorruptionSnafu, InvalidQuerySnafu, LocalDeepResearch,
    OfflineFixture, PageContent, PermanentIoSnafu, ProvenanceEntry, Provider, ProviderFailureSnafu,
    ProviderSpend, ProviderTier, QueryGenerator, QueryShape, QuotaExhaustedSnafu, RateLimitedSnafu,
    ResearchResult, ResearchStatus, Result, ResultHit, SearchConstraints, SourceKind,
    SourceRetriever, Synthesizer, TaskId, TimeoutSnafu, TransientIoSnafu, UnauthorizedSnafu,
    UnsupportedSnafu,
};
pub use synopsis as briefing;
