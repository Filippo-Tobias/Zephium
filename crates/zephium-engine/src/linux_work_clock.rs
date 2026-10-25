use std::{sync::OnceLock, time::Instant};
use zephium_agentic::AgentPolicyInstant;

/// Process-monotonic Work clock, initialized only at the explicit Work edge.
/// Original core deadlines must use this domain; no lease rebases its origin.
pub fn work_browser_monotonic_now() -> Option<AgentPolicyInstant> {
    u64::try_from(
        WORK_CLOCK_ORIGIN
            .get_or_init(Instant::now)
            .elapsed()
            .as_millis(),
    )
    .ok()
    .map(AgentPolicyInstant::from_millis)
}

static WORK_CLOCK_ORIGIN: OnceLock<Instant> = OnceLock::new();

/// Projects an existing absolute deadline into the same epoch as native Work
/// admission. Fractional milliseconds round up because policy time is discrete;
/// the original `Instant` remains the independently enforced execution deadline.
/// This never starts a new timeout or adds an arbitrary grace period.
pub fn work_browser_monotonic_deadline(deadline: Instant) -> Option<AgentPolicyInstant> {
    project_work_deadline(*WORK_CLOCK_ORIGIN.get_or_init(Instant::now), deadline)
}

fn project_work_deadline(origin: Instant, deadline: Instant) -> Option<AgentPolicyInstant> {
    let nanos = deadline.checked_duration_since(origin)?.as_nanos();
    u64::try_from(nanos.div_ceil(1_000_000))
        .ok()
        .map(AgentPolicyInstant::from_millis)
}
