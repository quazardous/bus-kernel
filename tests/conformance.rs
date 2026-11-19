//! The kernel runs the conformance suite on itself: bare (the Journal,
//! Relay and crossing checks skipped) and with an in-memory host journal
//! and relay (every check run, the crossing group included).

#![cfg(feature = "conformance")]

#[path = "support/memory.rs"]
mod memory;

use std::sync::Arc;

use bus_kernel::bus_contract::Bus;
use bus_kernel::conformance::{self, Contract, Outcome};
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
        conformance::checks()
            .iter()
            .any(|c| c.name == *n && matches!(c.contract, Contract::Journal | Contract::Crossing))
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
fn every_contract_runs_on_its_own() {
    for contract in Contract::ALL {
        conformance::run_contract(contract, hosted);
        let report = conformance::report_contract(contract, hosted);
        assert!(!report.is_empty(), "{contract}");
        for (name, outcome) in report {
            assert_eq!(outcome, Outcome::Passed, "{name}");
        }
    }
    let crossing = conformance::checks_of(Contract::Crossing).count();
    assert_eq!(crossing, 15, "the crossing group");
    let grouped: usize = Contract::ALL
        .iter()
        .map(|c| conformance::checks_of(*c).count())
        .sum();
    assert_eq!(
        grouped,
        conformance::checks().len(),
        "each check in one group"
    );
}

#[test]
fn checks_are_grouped_in_contract_order() {
    let order: Vec<_> = conformance::checks().iter().map(|c| c.contract).collect();
    let mut sorted = order.clone();
    sorted.sort_by_key(|c| Contract::ALL.iter().position(|a| a == c));
    assert_eq!(order, sorted);
}

/// The ids come from the guarantees page vendored into the crate
/// (`src/conformance/GUARANTEES.md`, a copy of bus-contract's).
#[test]
fn every_check_proves_properties_of_the_guarantees_page() {
    let known = conformance::properties();
    assert!(known.len() > 50, "the page parsed: {known:?}");
    for id in &known {
        assert_eq!(
            known.iter().filter(|k| *k == id).count(),
            1,
            "{id} is listed once"
        );
    }
    for check in conformance::checks() {
        assert!(!check.proves.is_empty(), "{} proves nothing", check.name);
        for id in check.proves {
            assert!(
                known.contains(id),
                "{} names {id}, not on the guarantees page",
                check.name
            );
        }
        assert!(
            check
                .proves
                .iter()
                .any(|id| id.starts_with(&format!("{}.", check.contract))),
            "{} proves nothing of its own contract",
            check.name
        );
    }
}

/// The page's "Checked by" column names checks that exist.
#[test]
fn the_guarantees_page_names_real_checks() {
    let names: Vec<_> = conformance::checks().iter().map(|c| c.name).collect();
    for line in conformance::GUARANTEES
        .lines()
        .filter(|l| l.starts_with("| `"))
    {
        let checked_by = line.trim_end_matches('|').rsplit('|').next().unwrap();
        for (i, part) in checked_by.split('`').enumerate() {
            if i % 2 == 1 && part.contains('.') && !part.contains(' ') && part.contains('_') {
                assert!(
                    names.contains(&part),
                    "the page names `{part}`, not a check"
                );
            }
        }
    }
}

#[test]
fn the_summary_names_contracts_checks_and_properties() {
    let text = conformance::summary(hosted);
    assert!(text.contains("crossing\n"));
    assert!(text.contains("order.cycle_refused_unchanged: passed"));
    assert!(text.contains("proves core.order.cycle-refused"));
    assert!(text.contains("not proved by a check"));
    assert!(text.contains("timings.cost.low"));
    assert!(!text.contains("FAILED"));
}
