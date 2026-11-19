//! The kernel runs the conformance suite on itself: bare (the Journal,
//! Relay and crossing checks skipped) and with an in-memory host journal
//! and relay (every check run, the crossing group included).

#![cfg(feature = "conformance")]

#[path = "support/memory.rs"]
mod memory;

use std::sync::Arc;

use bus_kernel::bus_contract::Bus;
use bus_kernel::conformance::{self, Outcome};
use bus_kernel::{Capability, Kernel};

fn bare() -> Arc<dyn Bus> {
    Arc::new(Kernel::new())
}

fn hosted() -> Arc<dyn Bus> {
    memory::full()
}

#[test]
fn the_bare_kernel_keeps_the_contract() {
    conformance::run(bare);
}

#[test]
fn the_kernel_with_a_host_journal_keeps_the_contract() {
    conformance::run(hosted);
}

#[test]
fn checks_needing_a_missing_capability_are_skipped() {
    let report = conformance::report(bare);
    let skipped: Vec<_> = report
        .iter()
        .filter(|(_, o)| matches!(o, Outcome::Skipped(_)))
        .map(|(name, _)| *name)
        .collect();
    assert!(!skipped.is_empty());
    assert!(skipped.iter().all(|n| {
        ["journal.", "relay.", "crossing."]
            .iter()
            .any(|g| n.starts_with(g))
    }));
    assert!(
        conformance::report(hosted)
            .iter()
            .all(|(_, o)| *o == Outcome::Passed),
        "every check runs on the full kernel"
    );
    assert!(
        conformance::checks()
            .iter()
            .any(|c| c.needs == [Capability::Defer])
    );
}

#[test]
fn every_crossing_check_runs_and_passes_with_a_relay() {
    let crossing: Vec<_> = conformance::report(hosted)
        .into_iter()
        .filter(|(name, _)| name.starts_with("crossing."))
        .collect();
    assert_eq!(crossing.len(), 11, "the crossing group");
    for (name, outcome) in crossing {
        assert_eq!(outcome, Outcome::Passed, "{name}");
    }
}
