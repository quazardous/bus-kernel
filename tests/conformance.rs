//! The kernel runs the conformance suite on itself: bare (the Journal,
//! Relay and crossing checks skipped) and with an in-memory host journal
//! and relay (every check run, the crossing group included).

#![cfg(feature = "conformance")]

#[path = "support/memory.rs"]
mod memory;

use std::sync::Arc;

use bus_kernel::Kernel;
use bus_kernel::bus_contract::Bus;
use bus_kernel::conformance::{self, Contract, Outcome};
#[allow(unused_imports)]
use bus_kernel::{BusCrossing, BusDefer, BusJournal, BusTimings};

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
            .any(|c| c.needs == [&bus_kernel::bus_contract_defer::CONTRACT])
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
#[test]
fn the_extension_traits_and_the_lookup_agree() {
    let check = conformance::checks()
        .iter()
        .find(|c| c.name == "capability.extension_and_lookup_agree")
        .expect("the check exists");
    assert_eq!(check.run(bare), Outcome::Passed);
    assert_eq!(check.run(hosted), Outcome::Passed);
}

/// An application's own points, declared on its bus before the suite
/// runs: an event, a journaled and relayable event, a request and an
/// alter.
mod app {
    use bus_kernel::bus_contract::{Point, PointId, PointKind};

    pub struct Opened;
    impl Point for Opened {
        const ID: PointId = PointId::new("app.opened");
        const KIND: PointKind = PointKind::Event;
        type Input = String;
        type Value = ();
    }

    pub struct Synced;
    impl Point for Synced {
        const ID: PointId = PointId::new("app.synced");
        const KIND: PointKind = PointKind::Event;
        type Input = u32;
        type Value = ();
    }

    impl bus_kernel::bus_contract_crossing::Relayed for Synced {
        fn encode(input: &u32) -> String {
            input.to_string()
        }
        fn decode(text: &str) -> Result<u32, String> {
            text.parse().map_err(|e| format!("{e}"))
        }
    }

    pub struct Close;
    impl Point for Close {
        const ID: PointId = PointId::new("app.close");
        const KIND: PointKind = PointKind::Request;
        type Input = String;
        type Value = Vec<String>;
    }

    pub struct Title;
    impl Point for Title {
        const ID: PointId = PointId::new("app.title");
        const KIND: PointKind = PointKind::Alter;
        type Input = ();
        type Value = String;
    }
}

const SYNCED_CODEC: bus_kernel::bus_contract_crossing::Codec =
    bus_kernel::bus_contract_crossing::Codec::of::<app::Synced>();
const SYNCED_RELAYABLE: &[bus_kernel::bus_contract::Facet] =
    &[bus_kernel::bus_contract_crossing::relayable(&SYNCED_CODEC)];

fn application() -> Arc<dyn Bus> {
    use bus_kernel::bus_contract::{ListenOpts, PointDecl, declare_all};
    let bus = hosted();
    let table = [
        PointDecl::of::<app::Opened>("A document opened."),
        PointDecl::of::<app::Synced>("Synced elsewhere.").facets(SYNCED_RELAYABLE),
        PointDecl::of::<app::Close>("May a document close?").fail_closed(),
        PointDecl::of::<app::Title>("The window title."),
    ];
    assert!(declare_all(&*bus, &table).is_empty());
    bus.listen::<app::Opened>("app.log", ListenOpts::new(), |_, _| Ok(()))
        .unwrap();
    bus
}

/// The verdicts do not depend on the application's other points.
#[test]
fn the_suite_passes_on_a_bus_holding_application_points() {
    for (name, outcome) in conformance::report(application) {
        assert_eq!(outcome, Outcome::Passed, "{name}");
    }
    conformance::run(application);
}
