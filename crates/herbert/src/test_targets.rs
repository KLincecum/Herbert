//! Opt-in native test targets. Ignored native tests reach a server only through endpoints and
//! saved-profile ids set in the environment; the defaults are unreachable placeholders.

/// Supported version lanes.
const LANES: [&str; 2] = ["2026.1", "2025.2"];

/// The endpoint and saved-profile id for a version lane such as "2026.1", from
/// HERBERT_TEST_ENDPOINT_2026_1 and HERBERT_TEST_PROFILE_2026_1.
pub(crate) fn lane(lane: &str) -> (&'static str, &'static str) {
    assert!(LANES.contains(&lane), "Only configured test targets are permitted");
    let key = lane.replace('.', "_");
    let dashed = lane.replace('.', "-");
    let read = |name: &str, default: String| -> &'static str {
        Box::leak(std::env::var(format!("{name}_{key}")).unwrap_or(default).into_boxed_str())
    };
    (
        read("HERBERT_TEST_ENDPOINT", format!("https://epicor-{dashed}.example.test/")),
        read("HERBERT_TEST_PROFILE", format!("test-{dashed}")),
    )
}

/// The lane whose saved-profile id is `profile`.
pub(crate) fn lane_for_profile(profile: &str) -> Option<&'static str> {
    LANES.into_iter().find(|l| lane(l).1 == profile)
}

/// Every configured endpoint.
pub(crate) fn endpoints() -> Vec<&'static str> {
    LANES.iter().map(|l| lane(l).0).collect()
}
