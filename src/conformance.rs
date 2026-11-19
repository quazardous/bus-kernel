//! The conformance suite: what bus-contract promises, checked on any
//! [`Bus`] implementation through the contract alone.
//!
//! An implementation runs it from its tests, with this crate as a
//! dev-dependency and the `conformance` feature on:
//!
//! ```ignore
//! #[test]
//! fn my_bus_keeps_the_contract() {
//!     bus_kernel::conformance::run(|| std::sync::Arc::new(MyBus::new()));
//! }
//! ```
//!
//! The checks are grouped by contract ([`Contract`]): `core`, `defer`,
//! `timings`, `journal` and `crossing`, as bus-contract's guarantees
//! page splits them. [`run`] runs every group; [`run_contract`] runs
//! one, for an implementation that offers a capability and wants its
//! checks alone. Each check names the property ids of the guarantees
//! page it proves ([`Check::proves`]); the page ships with this crate
//! ([`GUARANTEES`], [`properties`]), and [`summary`] writes what each
//! check gave with its ids, then the properties no check proves.
//!
//! ```ignore
//! #[test]
//! fn my_journal_keeps_its_contract() {
//!     use bus_kernel::conformance::{Contract, run_contract};
//!     run_contract(Contract::Journal, || std::sync::Arc::new(MyBus::new()));
//! }
//! ```
//!
//! Each check builds the buses it needs with `make`, fresh ones, so a
//! check never sees another's points. A check needing a capability the
//! bus does not offer ([`Capability`]) is skipped, not failed. Times are
//! checked as lower bounds or with wide margins: a loaded machine makes
//! runs longer, never shorter.

use std::cell::RefCell;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, ThreadId};
use std::time::Duration;

use web_time::Instant;

use bus_contract::{
    Answer, Bus, Capability, Codec, Cursor, FailureKind, HookError, IngestError, LAST, ListenOpts,
    Lost, Origin, OverBudget, Point, PointDecl, PointKey, PointKind, PointSpec, Refused, Relayed,
    RelayedAnswer, RelayedAsk, RelayedEntry, RelayedRequest, declare_all, far_listener, offers,
    order_dump,
};

/// Builds a fresh bus for a check.
pub type Make = fn() -> Arc<dyn Bus>;

/// A part of bus-contract, as its guarantees page splits it: each check
/// belongs to one, and a group can be run on its own
/// ([`run_contract`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Contract {
    /// The `Bus` trait, offered by every implementation.
    Core,
    /// The `Defer` capability.
    Defer,
    /// The `Timings` capability.
    Timings,
    /// The `Journal` capability.
    Journal,
    /// The `Relay` capability, on top of the journal.
    Crossing,
}

impl Contract {
    /// Every contract, in the guarantees page's order.
    pub const ALL: [Contract; 5] = [
        Contract::Core,
        Contract::Defer,
        Contract::Timings,
        Contract::Journal,
        Contract::Crossing,
    ];

    /// Its name, the first part of its property ids: `core`, `defer`,
    /// `timings`, `journal`, `crossing`.
    pub const fn name(self) -> &'static str {
        match self {
            Contract::Core => "core",
            Contract::Defer => "defer",
            Contract::Timings => "timings",
            Contract::Journal => "journal",
            Contract::Crossing => "crossing",
        }
    }

    /// The capabilities a bus offers when it offers this contract.
    pub const fn needs(self) -> &'static [Capability] {
        match self {
            Contract::Core => &[],
            Contract::Defer => &[Capability::Defer],
            Contract::Timings => &[Capability::Timings],
            Contract::Journal => &[Capability::Journal],
            Contract::Crossing => &[Capability::Journal, Capability::Relay],
        }
    }
}

impl fmt::Display for Contract {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The guarantees page of bus-contract (`GUARANTEES.md`), as this
/// release of the suite was written against: the source of
/// [`properties`].
pub const GUARANTEES: &str = include_str!("conformance/GUARANTEES.md");

/// Every property id of the guarantees page ([`GUARANTEES`]), in its
/// order: the first cell of each table row, `<contract>.<area>.<property>`.
pub fn properties() -> Vec<&'static str> {
    GUARANTEES
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split_once("` |").map(|(id, _)| id))
        .collect()
}

/// The property ids of the guarantees page no check proves: stated, a
/// non-guarantee, or a matter for benchmarks.
pub fn unproved() -> Vec<&'static str> {
    properties()
        .into_iter()
        .filter(|id| !checks().iter().any(|c| c.proves.contains(id)))
        .collect()
}

/// One check of the suite.
#[derive(Clone, Copy)]
pub struct Check {
    /// Its name, stable: `group.what`.
    pub name: &'static str,
    /// The capabilities it needs; without one, it is skipped.
    pub needs: &'static [Capability],
    /// The contract it belongs to.
    pub contract: Contract,
    /// The property ids of the guarantees page ([`properties`]) it
    /// proves, wholly or in part.
    pub proves: &'static [&'static str],
    body: fn(Make),
}

impl fmt::Debug for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Check")
            .field("name", &self.name)
            .field("needs", &self.needs)
            .field("contract", &self.contract)
            .field("proves", &self.proves)
            .finish_non_exhaustive()
    }
}

/// What a check gave on a bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It held.
    Passed,
    /// The bus does not offer this capability: not run.
    Skipped(Capability),
    /// It did not hold: the assertion's message.
    Failed(String),
}

impl Check {
    /// Runs the check on buses from `make`, its panic caught as a
    /// failure.
    pub fn run(&self, make: Make) -> Outcome {
        let probe = make();
        if let Some(missing) = self.needs.iter().find(|c| !offers(&*probe, **c)) {
            return Outcome::Skipped(*missing);
        }
        drop(probe);
        match catch_unwind(AssertUnwindSafe(|| (self.body)(make))) {
            Ok(()) => Outcome::Passed,
            Err(payload) => Outcome::Failed(
                payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .unwrap_or_else(|| "a panic without a message".to_owned()),
            ),
        }
    }
}

/// Every check, grouped by contract in [`Contract::ALL`]'s order.
pub fn checks() -> &'static [Check] {
    CHECKS
}

/// The checks of one contract, in order.
pub fn checks_of(contract: Contract) -> impl Iterator<Item = &'static Check> {
    checks().iter().filter(move |c| c.contract == contract)
}

/// Runs every check and returns each one's outcome, in order.
pub fn report(make: Make) -> Vec<(&'static str, Outcome)> {
    checks().iter().map(|c| (c.name, c.run(make))).collect()
}

/// Runs one contract's checks and returns each one's outcome, in order.
pub fn report_contract(contract: Contract, make: Make) -> Vec<(&'static str, Outcome)> {
    checks_of(contract).map(|c| (c.name, c.run(make))).collect()
}

/// Runs every check; panics naming each failed one, the properties it
/// proves, and why.
pub fn run(make: Make) {
    fail_on(checks().iter(), make);
}

/// Runs one contract's checks; panics naming each failed one, the
/// properties it proves, and why. A bus without the contract's
/// capabilities skips them all.
pub fn run_contract(contract: Contract, make: Make) {
    fail_on(checks_of(contract), make);
}

fn fail_on<'a>(checks: impl Iterator<Item = &'a Check>, make: Make) {
    let failed: Vec<String> = checks
        .filter_map(|c| match c.run(make) {
            Outcome::Failed(why) => Some(format!("- {} [{}]: {why}", c.name, c.proves.join(", "))),
            _ => None,
        })
        .collect();
    assert!(
        failed.is_empty(),
        "{} conformance check(s) failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// Runs every check and writes what each gave, by contract, with the
/// properties it proves, then the properties no check proves: a text
/// for a person reading a test's output.
pub fn summary(make: Make) -> String {
    use fmt::Write as _;
    let mut out = String::new();
    for contract in Contract::ALL {
        let _ = writeln!(out, "{contract}");
        for c in checks_of(contract) {
            let outcome = match c.run(make) {
                Outcome::Passed => "passed".to_owned(),
                Outcome::Skipped(cap) => format!("skipped (no {cap:?})"),
                Outcome::Failed(why) => format!("FAILED: {why}"),
            };
            let _ = writeln!(out, "  {}: {outcome}", c.name);
            let _ = writeln!(out, "    proves {}", c.proves.join(", "));
        }
    }
    let _ = writeln!(out, "not proved by a check");
    for id in unproved() {
        let _ = writeln!(out, "  {id}");
    }
    out
}

/// A check of `contract`, run when the bus offers it.
const fn check(
    contract: Contract,
    name: &'static str,
    proves: &'static [&'static str],
    body: fn(Make),
) -> Check {
    Check {
        name,
        needs: contract.needs(),
        contract,
        proves,
        body,
    }
}

use Contract::{Core, Crossing, Defer as Deferred, Journal as Journaled, Timings as Timed};

const CHECKS: &[Check] = &[
    // core
    check(
        Core,
        "declare.once_and_typed",
        &["core.declare.once-typed"],
        declare_once_and_typed,
    ),
    check(
        Core,
        "declare.missing_capability_refused",
        &["core.declare.table-checked"],
        missing_capability_refused,
    ),
    check(
        Core,
        "order.constraints_priority_registration",
        &["core.order.declared"],
        order_constraints_priority_registration,
    ),
    check(
        Core,
        "order.cycle_refused_unchanged",
        &["core.order.cycle-refused"],
        order_cycle_refused_unchanged,
    ),
    check(
        Core,
        "order.missing_target_reported_ignored",
        &["core.order.missing-target"],
        order_missing_target_reported_ignored,
    ),
    check(
        Core,
        "order.last_runs_after_all_alone",
        &["core.order.last"],
        order_last_runs_after_all_alone,
    ),
    check(
        Core,
        "emit.alter_keeps_successful_edits",
        &["core.alter.chain", "core.failure.isolated"],
        alter_keeps_successful_edits,
    ),
    check(
        Core,
        "emit.collect_gathers_fresh_parts",
        &["core.collect.fresh-parts"],
        collect_gathers_fresh_parts,
    ),
    check(
        Core,
        "emit.failure_sinks_hear_dropped_only",
        &["core.failure.isolated", "core.failure.sinks"],
        failure_sinks_hear_dropped_only,
    ),
    check(
        Core,
        "emit.reentrant_and_cross_thread",
        &["core.reentrant.no-lock", "core.thread.shared"],
        reentrant_and_cross_thread,
    ),
    check(
        Core,
        "emit.listen_on_the_emitting_point",
        &["core.reentrant.no-lock"],
        listen_on_the_emitting_point,
    ),
    check(
        Core,
        "request.refusal_stops_not_a_failure",
        &["core.request.veto-stops"],
        refusal_stops_not_a_failure,
    ),
    check(
        Core,
        "request.refusal_off_a_request_fails",
        &[
            "core.request.refuse-elsewhere-fails",
            "core.kind.not-declarative",
        ],
        refusal_off_a_request_fails,
    ),
    check(
        Core,
        "request.fail_closed_refuses",
        &["core.request.fail-closed"],
        fail_closed_refuses,
    ),
    check(
        Core,
        "request.refusal_keeps_earlier_effects",
        &["core.request.no-rollback"],
        refusal_keeps_earlier_effects,
    ),
    check(
        Core,
        "remove.unlisten_and_forget",
        &["core.remove.no-run-after"],
        unlisten_and_forget,
    ),
    check(
        Core,
        "local.ordered_and_queued",
        &["core.local.bound", "core.local.queued"],
        local_ordered_and_queued,
    ),
    check(
        Core,
        "local.error_boundary",
        &["core.local.error-boundary"],
        local_error_boundary,
    ),
    check(
        Core,
        "local.forget_from_any_thread",
        &["core.remove.no-run-after"],
        local_forget_from_any_thread,
    ),
    check(
        Core,
        "local.deferred_collect_fresh",
        &["core.local.queued"],
        local_deferred_collect_fresh,
    ),
    check(
        Core,
        "local.refused_request_queues_nothing",
        &["core.request.veto-stops"],
        local_refused_request_queues_nothing,
    ),
    check(
        Core,
        "local.queued_emits_run_in_order",
        &["core.local.queued"],
        local_queued_emits_run_in_order,
    ),
    check(
        Core,
        "local.panicking_waker_ignored",
        &["core.local.queued"],
        local_panicking_waker_ignored,
    ),
    check(
        Core,
        "deadline.inline_late_fails",
        &["core.deadline.inline-late"],
        deadline_inline_late_fails,
    ),
    check(
        Core,
        "deadline.threaded_abandoned",
        &["core.deadline.threaded-abandoned"],
        deadline_threaded_abandoned,
    ),
    check(
        Core,
        "introspect.points_listeners_counters",
        &["core.introspect.points"],
        introspect_points_listeners_counters,
    ),
    check(
        Core,
        "capability.getter_and_lookup_agree",
        &["core.capability.lookup-agrees"],
        capability_getter_and_lookup_agree,
    ),
    // defer
    check(
        Deferred,
        "defer.drain_in_ask_order",
        &[
            "defer.ask.returns-at-once",
            "defer.drain.ask-order",
            "defer.drain.outcome-reported",
            "defer.drain.later-asks-wait",
        ],
        defer_drain_in_ask_order,
    ),
    check(
        Deferred,
        "defer.asks_per_thread",
        &["defer.thread.per-thread"],
        defer_asks_per_thread,
    ),
    check(
        Deferred,
        "defer.ask_wakes_its_thread",
        &["defer.ask.returns-at-once"],
        defer_ask_wakes_its_thread,
    ),
    check(
        Deferred,
        "defer.listeners_read_at_the_drain",
        &["defer.drain.ask-order"],
        defer_listeners_read_at_the_drain,
    ),
    // timings
    check(
        Timed,
        "timings.timed_until_reset",
        &["timings.runs.timed", "timings.stats.exact-counts"],
        timings_timed_until_reset,
    ),
    check(
        Timed,
        "timings.over_budget_reported",
        &["timings.budget.reported-not-failed"],
        timings_over_budget_reported,
    ),
    check(
        Timed,
        "timings.threaded_late_counts_the_wait",
        &["timings.runs.timed"],
        timings_threaded_late_counts_the_wait,
    ),
    // journal
    check(
        Journaled,
        "journal.refuses_off_journaled",
        &[
            "journal.publish.journaled-only",
            "core.kind.not-declarative",
        ],
        journal_refuses_off_journaled,
    ),
    check(
        Journaled,
        "journal.delivered_in_order_off_publisher",
        &[
            "journal.seq.ordered",
            "journal.publish.never-waits",
            "journal.delivery.once-in-order",
            "journal.read.since",
        ],
        journal_delivered,
    ),
    check(
        Journaled,
        "journal.numbers_grow_by_one",
        &["journal.seq.ordered"],
        journal_numbers_grow_by_one,
    ),
    check(
        Journaled,
        "journal.since_reports_the_gap",
        &["journal.read.since"],
        journal_since_reports_the_gap,
    ),
    check(
        Journaled,
        "journal.wait_and_wait_for",
        &["journal.read.wait-flush"],
        journal_wait_and_wait_for,
    ),
    check(
        Journaled,
        "journal.cause_kept",
        &["journal.read.wait-flush"],
        journal_cause_kept,
    ),
    // crossing
    check(
        Crossing,
        "relay.resume_and_ingest",
        &[
            "crossing.resume.cursor-order",
            "crossing.ingest.delivered",
            "crossing.origin.from-link",
        ],
        relay_resume_and_ingest,
    ),
    Check {
        // Run on every bus: one that offers `Relay` without `Journal`
        // would skip the others.
        needs: &[],
        ..check(
            Crossing,
            "crossing.relay_implies_journal",
            &["crossing.relay.implies-journal"],
            crossing_relay_implies_journal,
        )
    },
    check(
        Crossing,
        "crossing.declared_requests_fail_closed_alter_never",
        &[
            "crossing.declare.what-crosses",
            "core.declare.table-checked",
            "core.request.fail-closed",
            "core.kind.not-declarative",
        ],
        crossing_declared,
    ),
    check(
        Crossing,
        "crossing.answered_far_means_the_same",
        &["crossing.answer.same-meaning"],
        crossing_answered_far_means_the_same,
    ),
    check(
        Crossing,
        "crossing.veto_carries_its_reason",
        &["crossing.answer.veto-reason"],
        crossing_veto_reason,
    ),
    check(
        Crossing,
        "crossing.no_answer_in_time_refuses",
        &["crossing.deadline.fail-closed"],
        crossing_no_answer,
    ),
    check(
        Crossing,
        "crossing.far_side_keeps_the_deadline",
        &["crossing.deadline.far-side-keeps"],
        crossing_far_deadline,
    ),
    check(
        Crossing,
        "crossing.answered_once_per_origin",
        &["crossing.ask.once-per-origin"],
        crossing_answered_once,
    ),
    check(
        Crossing,
        "crossing.not_a_relayable_request_refused",
        &["crossing.answer.refuses-unknown"],
        crossing_not_relayable_refused,
    ),
    check(
        Crossing,
        "crossing.origin_kept_on_entries",
        &["crossing.origin.from-link"],
        crossing_origin_kept,
    ),
    check(
        Crossing,
        "crossing.authority_checks_asks_first",
        &["crossing.authority.first"],
        crossing_authority_asks,
    ),
    check(
        Crossing,
        "crossing.authority_checks_events_panic_refuses",
        &["crossing.authority.first"],
        crossing_authority_events,
    ),
    check(
        Crossing,
        "crossing.same_version_only",
        &[
            "crossing.version.same-only",
            "crossing.ingest.delivered",
            "crossing.answer.refuses-unknown",
        ],
        crossing_same_version,
    ),
    check(
        Crossing,
        "crossing.lost_entries_reported",
        &["crossing.resume.lost-reported"],
        crossing_lost_entries_reported,
    ),
    check(
        Crossing,
        "crossing.ingested_not_echoed",
        &["crossing.ingest.no-echo"],
        crossing_ingested_not_echoed,
    ),
];

// --- Points ---

struct Trail;
impl Point for Trail {
    const ID: &'static str = "conformance.trail";
    type Input = ();
    type Value = Vec<String>;
}

struct Parts;
impl Point for Parts {
    const ID: &'static str = "conformance.parts";
    type Input = u32;
    type Value = Vec<u32>;
}

/// `Trail`'s id, another type.
struct Impostor;
impl Point for Impostor {
    const ID: &'static str = "conformance.trail";
    type Input = ();
    type Value = u8;
}

struct Run;
impl Point for Run {
    const ID: &'static str = "conformance.run";
    const KIND: PointKind = PointKind::Request;
    type Input = String;
    type Value = Vec<String>;
}

struct Step;
impl Point for Step {
    const ID: &'static str = "conformance.step";
    const KIND: PointKind = PointKind::Event;
    type Input = u32;
    type Value = ();
}

struct Plain;
impl Point for Plain {
    const ID: &'static str = "conformance.plain";
    const KIND: PointKind = PointKind::Event;
    type Input = String;
    type Value = ();
}

struct Shared;
impl Point for Shared {
    const ID: &'static str = "conformance.shared";
    const KIND: PointKind = PointKind::Event;
    type Input = u32;
    type Value = ();
}

impl Relayed for Shared {
    fn encode(input: &u32) -> String {
        input.to_string()
    }
    fn decode(text: &str) -> Result<u32, String> {
        text.parse().map_err(|e| format!("{e}"))
    }
}

const WAIT: Duration = Duration::from_secs(10);
const MS: Duration = Duration::from_millis(1);

fn push(bus: &dyn Bus, id: &'static str, opts: ListenOpts) {
    bus.listen::<Trail>(id, opts, move |_, v| {
        v.push(id.to_owned());
        Ok(())
    })
    .unwrap();
}

fn ids(bus: &dyn Bus, point: &str) -> Vec<String> {
    bus.listeners(point)
        .unwrap_or_default()
        .into_iter()
        .map(|l| l.id)
        .collect()
}

// --- Declarations ---

fn declare_once_and_typed(make: Make) {
    let bus = make();
    push(&*bus, "a", ListenOpts::new());
    assert!(
        matches!(
            bus.listen::<Trail>("a", ListenOpts::new(), |_, _| Ok(())),
            Err(Refused::Duplicate { .. })
        ),
        "a listener id taken on the point is refused"
    );
    assert!(
        matches!(
            bus.listen::<Impostor>("x", ListenOpts::new(), |_, _| Ok(())),
            Err(Refused::TypeMismatch { .. })
        ),
        "a listener under another type is refused"
    );
    assert!(
        matches!(
            bus.declare::<Impostor>(PointSpec::new("no")),
            Err(Refused::TypeMismatch { .. })
        ),
        "a declaration under another type is refused"
    );
    let out = bus.alter::<Impostor>((), 7);
    assert_eq!(out.value, 7, "an emit under another type runs nothing");
    assert_eq!(out.failures[0].kind, FailureKind::TypeMismatch);
    bus.declare::<Trail>(PointSpec::new("trail")).unwrap();
    assert!(
        matches!(
            bus.declare::<Trail>(PointSpec::new("again")),
            Err(Refused::AlreadyDeclared { .. })
        ),
        "a point is declared once"
    );
}

fn missing_capability_refused(make: Make) {
    let bus = make();
    for capability in [
        Capability::Defer,
        Capability::Timings,
        Capability::Journal,
        Capability::Relay,
    ] {
        if offers(&*bus, capability) {
            continue;
        }
        let needs: &'static [Capability] = match capability {
            Capability::Defer => &[Capability::Defer],
            Capability::Timings => &[Capability::Timings],
            Capability::Journal => &[Capability::Journal],
            _ => &[Capability::Relay],
        };
        let table = [PointDecl::of::<Trail>("needs").needs(needs)];
        let refused = declare_all(&*bus, &table);
        assert!(
            matches!(&refused[..], [Refused::Unsupported { capability: c, .. }] if *c == capability),
            "a point needing {capability:?} is refused: {refused:?}"
        );
    }
    if !offers(&*bus, Capability::Journal) {
        assert!(
            matches!(
                bus.declare::<Step>(PointSpec::new("steps").journaled()),
                Err(Refused::Unsupported { .. })
            ),
            "a journaled point without the Journal capability is refused"
        );
    }
}

// --- Order ---

fn order_constraints_priority_registration(make: Make) {
    let bus = make();
    push(&*bus, "late", ListenOpts::new().after("core"));
    push(&*bus, "core", ListenOpts::new());
    push(&*bus, "first", ListenOpts::new().priority(5));
    push(
        &*bus,
        "guard",
        ListenOpts::new().before("core").priority(-9),
    );
    push(&*bus, "tail", ListenOpts::new());
    let out = bus.alter::<Trail>((), Vec::new());
    assert!(out.failures.is_empty(), "{:?}", out.failures);
    assert_eq!(out.value, ["first", "tail", "guard", "core", "late"]);
    assert_eq!(
        ids(&*bus, Trail::ID),
        out.value,
        "introspection, same order"
    );
}

fn order_cycle_refused_unchanged(make: Make) {
    let bus = make();
    push(&*bus, "a", ListenOpts::new().after("b"));
    let refused = bus
        .listen::<Trail>("b", ListenOpts::new().after("a"), |_, _| Ok(()))
        .unwrap_err();
    assert!(matches!(refused, Refused::Cycle { .. }), "{refused:?}");
    assert_eq!(ids(&*bus, Trail::ID), ["a"], "nothing changed");
    assert!(
        matches!(
            bus.listen::<Trail>("me", ListenOpts::new().before("me"), |_, _| Ok(())),
            Err(Refused::Cycle { .. })
        ),
        "itself as a target is a cycle"
    );
}

fn order_missing_target_reported_ignored(make: Make) {
    let bus = make();
    push(&*bus, "a", ListenOpts::new().after("ghost").owner("m"));
    let out = bus.alter::<Trail>((), Vec::new());
    assert_eq!(out.value, ["a"]);
    assert_eq!(out.failures.len(), 1);
    assert_eq!(
        out.failures[0].kind,
        FailureKind::MissingTarget("ghost".into())
    );
    assert!(!out.failures[0].dropped(), "the listener still ran");
    assert_eq!(bus.listeners(Trail::ID).unwrap()[0].missing, ["ghost"]);
    push(&*bus, "ghost", ListenOpts::new().priority(-1));
    let out = bus.alter::<Trail>((), Vec::new());
    assert!(out.failures.is_empty());
    assert_eq!(out.value, ["ghost", "a"], "the constraint holds once there");
}

fn order_last_runs_after_all_alone(make: Make) {
    let bus = make();
    push(&*bus, "check", ListenOpts::new().last());
    push(&*bus, "early", ListenOpts::new().priority(10));
    push(
        &*bus,
        "late",
        ListenOpts::new().after("early").priority(-10),
    );
    let out = bus.alter::<Trail>((), Vec::new());
    assert_eq!(out.value, ["early", "late", "check"]);
    assert!(out.failures.is_empty(), "LAST is no missing target");
    let listeners = bus.listeners(Trail::ID).unwrap();
    assert!(listeners[2].after.iter().any(|a| a == LAST));
    assert!(
        matches!(
            bus.listen::<Trail>("check-too", ListenOpts::new().last(), |_, _| Ok(())),
            Err(Refused::Cycle { .. })
        ),
        "two LAST listeners are a cycle"
    );
}

// --- Emits ---

fn alter_keeps_successful_edits(make: Make) {
    let bus = make();
    push(&*bus, "one", ListenOpts::new());
    bus.listen::<Trail>("fails", ListenOpts::new(), |_, v| {
        v.push("half".into());
        Err("refused".into())
    })
    .unwrap();
    bus.listen::<Trail>("panics", ListenOpts::new(), |_, v| {
        v.clear();
        panic!("boom");
    })
    .unwrap();
    push(&*bus, "two", ListenOpts::new());
    let out = bus.alter::<Trail>((), vec!["start".into()]);
    assert_eq!(out.value, ["start", "one", "two"]);
    let kinds: Vec<_> = out.failures.iter().map(|f| f.kind.clone()).collect();
    assert_eq!(
        kinds,
        [
            FailureKind::Error("refused".into()),
            FailureKind::Panic("boom".into())
        ]
    );
    assert!(out.failures.iter().all(|f| f.dropped()));
}

fn collect_gathers_fresh_parts(make: Make) {
    let bus = make();
    bus.listen::<Parts>("double", ListenOpts::new(), |n, out| {
        out.push(n * 2);
        Ok(())
    })
    .unwrap();
    bus.listen::<Parts>("partial", ListenOpts::new().priority(1), |n, out| {
        out.push(*n);
        Err("incomplete".into())
    })
    .unwrap();
    bus.listen::<Parts>("square", ListenOpts::new(), |n, out| {
        assert!(out.is_empty(), "each listener starts from a fresh value");
        out.extend([n * n, 1]);
        Ok(())
    })
    .unwrap();
    let out = bus.collect::<Parts>(3);
    assert_eq!(
        out.parts,
        [
            ("double".to_string(), vec![6]),
            ("square".to_string(), vec![9, 1])
        ]
    );
    let failed: Vec<_> = out.failures.iter().map(|f| f.listener.as_str()).collect();
    assert_eq!(failed, ["partial"]);
}

fn failure_sinks_hear_dropped_only(make: Make) {
    let bus = make();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = seen.clone();
    bus.on_failure(move |f| sink.lock().unwrap().push(f.listener.clone()));
    bus.on_failure(|_| panic!("a broken sink"));
    push(&*bus, "ok", ListenOpts::new().after("ghost"));
    bus.listen::<Trail>("bad", ListenOpts::new(), |_, _| Err("no disk".into()))
        .unwrap();
    let out = bus.alter::<Trail>((), Vec::new());
    assert_eq!(out.failures.len(), 2);
    assert_eq!(out.value, ["ok"], "a panicking sink stops nothing");
    assert_eq!(*seen.lock().unwrap(), ["bad"]);
}

fn reentrant_and_cross_thread(make: Make) {
    let bus = make();
    bus.listen::<Parts>("n", ListenOpts::new(), |n, out| {
        out.push(*n);
        Ok(())
    })
    .unwrap();
    let inner = bus.clone();
    bus.listen::<Trail>("reenter", ListenOpts::new(), move |_, v| {
        let got = inner.collect::<Parts>(9);
        v.push(format!("{:?}", got.parts[0].1));
        inner
            .listen::<Parts>("added", ListenOpts::new(), |_, _| Ok(()))
            .map_err(|e| e.to_string().into())
    })
    .unwrap();
    let out = bus.alter::<Trail>((), Vec::new());
    assert!(out.failures.is_empty(), "{:?}", out.failures);
    assert_eq!(out.value, ["[9]"]);
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let bus = bus.clone();
            thread::spawn(move || bus.collect::<Parts>(i).parts.len())
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), 2);
    }
}

// --- Requests ---

fn guarded(bus: &dyn Bus, policy: impl Fn(&str) -> Result<(), HookError> + Send + Sync + 'static) {
    bus.listen::<Run>(
        "policy",
        ListenOpts::new().owner("policy"),
        move |cmd, did| {
            did.push("policy".into());
            policy(cmd)
        },
    )
    .unwrap();
    bus.listen::<Run>("handler", ListenOpts::new().after("policy"), |cmd, did| {
        did.push(format!("ran {cmd}"));
        Ok(())
    })
    .unwrap();
}

fn refusal_stops_not_a_failure(make: Make) {
    let bus = make();
    guarded(&*bus, |cmd| {
        if cmd == "delete" {
            Err(HookError::refuse("may not delete"))
        } else {
            Ok(())
        }
    });
    let sunk = Arc::new(Mutex::new(0));
    let s = sunk.clone();
    bus.on_failure(move |_| *s.lock().unwrap() += 1);
    assert_eq!(
        bus.alter::<Run>("open".into(), vec![]).value,
        ["policy", "ran open"]
    );
    let out = bus.alter::<Run>("delete".into(), vec![]);
    assert!(out.value.is_empty(), "the refusing edit is dropped too");
    assert!(out.failures.is_empty());
    let refusal = out.refused.expect("refused");
    assert_eq!(refusal.listener, "policy");
    assert_eq!(refusal.owner.as_deref(), Some("policy"));
    assert_eq!(refusal.reason, "may not delete");
    assert!(!refusal.failed);
    assert_eq!(
        *sunk.lock().unwrap(),
        0,
        "a refusal reaches no failure sink"
    );
    let out = bus.collect::<Run>("delete".into());
    assert!(out.parts.is_empty());
    assert_eq!(out.refused.expect("refused").reason, "may not delete");
}

fn refusal_off_a_request_fails(make: Make) {
    let bus = make();
    bus.listen::<Trail>("picky", ListenOpts::new(), |_, _| {
        Err(HookError::refuse("no"))
    })
    .unwrap();
    push(&*bus, "after", ListenOpts::new().after("picky"));
    let out = bus.alter::<Trail>((), vec![]);
    assert_eq!(out.value, ["after"]);
    assert!(out.refused.is_none());
    assert_eq!(out.failures[0].kind, FailureKind::Error("no".into()));
}

fn fail_closed_refuses(make: Make) {
    let bus = make();
    guarded(&*bus, |_| Err("unreadable".into()));
    let out = bus.alter::<Run>("delete".into(), vec![]);
    assert_eq!(out.value, ["ran delete"], "open: the failure is dropped");
    assert!(out.refused.is_none());

    let bus = make();
    bus.declare::<Run>(PointSpec::new("runs").fail_closed())
        .unwrap();
    guarded(&*bus, |_| Err("unreadable".into()));
    let out = bus.alter::<Run>("delete".into(), vec![]);
    assert!(out.value.is_empty(), "closed: the handler did not run");
    assert_eq!(out.failures.len(), 1, "the failure is still reported");
    let refusal = out.refused.expect("refused");
    assert!(refusal.failed);
    assert_eq!(refusal.reason, "unreadable");
    assert!(
        bus.points()
            .iter()
            .any(|p| p.id == Run::ID && p.fail_closed)
    );

    let bus = make();
    bus.declare::<Run>(PointSpec::new("runs").fail_closed())
        .unwrap();
    guarded(&*bus, |_| panic!("policy bug"));
    let out = bus.alter::<Run>("x".into(), vec![]);
    assert!(out.value.is_empty());
    assert_eq!(out.refused.expect("refused").reason, "policy bug");
}

// --- Removal ---

fn unlisten_and_forget(make: Make) {
    let bus = make();
    push(&*bus, "a", ListenOpts::new().owner("plugin"));
    push(&*bus, "b", ListenOpts::new());
    bus.listen::<Parts>("c", ListenOpts::new().owner("plugin"), |_, _| Ok(()))
        .unwrap();
    assert_eq!(bus.forget("plugin"), 2);
    assert!(bus.unlisten(Trail::ID, "b"));
    assert!(!bus.unlisten(Trail::ID, "b"));
    assert!(bus.alter::<Trail>((), Vec::new()).value.is_empty());
    assert!(bus.collect::<Parts>(1).parts.is_empty());
}

// --- Local listeners ---

fn local_ordered_and_queued(make: Make) {
    let bus = make();
    let seen: Rc<RefCell<Vec<Vec<String>>>> = Rc::default();
    push(&*bus, "core", ListenOpts::new());
    let mine = seen.clone();
    bus.listen_local::<Trail>(
        "ui.panel",
        ListenOpts::new().owner("ui").after("core"),
        move |_, v| {
            mine.borrow_mut().push(v.clone());
            v.push("ui.panel".into());
            Ok(())
        },
    )
    .unwrap();
    push(&*bus, "tail", ListenOpts::new().after("ui.panel"));
    let woken = Arc::new(Mutex::new(Vec::new()));
    let w = woken.clone();
    bus.on_pending(move |t| w.lock().unwrap().push(t));

    let out = bus.alter::<Trail>((), Vec::new());
    assert_eq!(
        out.value,
        ["core", "ui.panel", "tail"],
        "inline on its thread"
    );
    assert!(out.deferred.is_empty());
    assert!(bus.run_pending().is_empty());

    let other = bus.clone();
    let out = thread::spawn(move || other.alter::<Trail>((), Vec::new()))
        .join()
        .unwrap();
    assert_eq!(out.value, ["core", "tail"]);
    assert_eq!(out.deferred, ["ui.panel"]);
    assert_eq!(*woken.lock().unwrap(), [thread::current().id()]);
    let ran = bus.run_pending();
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0].ran, ["ui.panel"]);
    assert_eq!(seen.borrow()[1], ["core", "tail"]);
    assert!(bus.run_pending().is_empty());
    let infos = bus.listeners(Trail::ID).unwrap();
    assert!(infos[1].local);
    assert!(order_dump(&*bus, &[Trail::ID]).contains("ui.panel"));
    assert!(
        matches!(
            bus.listen_local::<Trail>("ui.slow", ListenOpts::new().threaded(), |_, _| Ok(())),
            Err(Refused::Threaded { .. })
        ),
        "a local listener cannot be threaded"
    );
}

fn local_error_boundary(make: Make) {
    let bus = make();
    bus.listen_local::<Trail>("ui.broken", ListenOpts::new(), |_, v| {
        v.push("half".into());
        panic!("boom")
    })
    .unwrap();
    push(&*bus, "core", ListenOpts::new());
    let out = bus.alter::<Trail>((), Vec::new());
    assert_eq!(out.value, ["core"]);
    assert!(matches!(&out.failures[0].kind, FailureKind::Panic(m) if m == "boom"));
    let other = bus.clone();
    thread::spawn(move || other.alter::<Trail>((), Vec::new()))
        .join()
        .unwrap();
    let ran = bus.run_pending();
    assert_eq!(ran[0].ran, ["ui.broken"]);
    assert_eq!(ran[0].failures.len(), 1);
}

fn local_forget_from_any_thread(make: Make) {
    let bus = make();
    let held = Rc::new(());
    let mine = held.clone();
    bus.listen_local::<Trail>("ui.panel", ListenOpts::new().owner("ui"), move |_, v| {
        let _ = &mine;
        v.push("ui.panel".into());
        Ok(())
    })
    .unwrap();
    let other = bus.clone();
    let (deferred, forgot) = thread::spawn(move || {
        let out = other.alter::<Trail>((), Vec::new());
        (out.deferred, other.forget("ui"))
    })
    .join()
    .unwrap();
    assert_eq!(deferred, ["ui.panel"]);
    assert_eq!(forgot, 1);
    let ran = bus.run_pending();
    assert!(
        ran.iter().all(|r| r.ran.is_empty()),
        "removed since the emit: not run"
    );
    assert_eq!(
        Rc::strong_count(&held),
        1,
        "its closure dropped on its thread"
    );
    assert!(ids(&*bus, Trail::ID).is_empty());
}

fn local_deferred_collect_fresh(make: Make) {
    let bus = make();
    let got: Rc<RefCell<Vec<Vec<u32>>>> = Rc::default();
    let mine = got.clone();
    bus.listen_local::<Parts>("ui.badge", ListenOpts::new(), move |n, out| {
        out.push(*n);
        mine.borrow_mut().push(out.clone());
        Ok(())
    })
    .unwrap();
    bus.listen::<Parts>("disk", ListenOpts::new(), |n, out| {
        out.push(n + 1);
        Ok(())
    })
    .unwrap();
    let other = bus.clone();
    let out = thread::spawn(move || other.collect::<Parts>(7))
        .join()
        .unwrap();
    assert_eq!(out.parts, [("disk".to_string(), vec![8])]);
    assert_eq!(out.deferred, ["ui.badge"]);
    assert_eq!(bus.run_pending()[0].ran, ["ui.badge"]);
    assert_eq!(*got.borrow(), [vec![7]]);
}

fn local_refused_request_queues_nothing(make: Make) {
    let bus = make();
    bus.listen_local::<Run>("ui.confirm", ListenOpts::new(), |cmd, did| {
        if cmd == "delete" {
            return Err(HookError::refuse("the user said no"));
        }
        did.push("confirmed".into());
        Ok(())
    })
    .unwrap();
    bus.listen_local::<Run>(
        "ui.after",
        ListenOpts::new().after("ui.confirm"),
        |_, did| {
            did.push("after".into());
            Ok(())
        },
    )
    .unwrap();
    bus.listen::<Run>("gate", ListenOpts::new().after("ui.after"), |cmd, _| {
        if cmd == "format" {
            Err(HookError::refuse("never"))
        } else {
            Ok(())
        }
    })
    .unwrap();
    let other = bus.clone();
    let out = thread::spawn(move || other.alter::<Run>("format".into(), vec![]))
        .join()
        .unwrap();
    assert_eq!(out.refused.expect("refused").listener, "gate");
    assert!(out.deferred.is_empty(), "a refused request queues nothing");
    assert!(bus.run_pending().is_empty());
    let other = bus.clone();
    let out = thread::spawn(move || other.alter::<Run>("delete".into(), vec![]))
        .join()
        .unwrap();
    assert_eq!(out.deferred, ["ui.confirm", "ui.after"]);
    let ran = bus.run_pending();
    assert_eq!(
        ran[0].ran,
        ["ui.confirm"],
        "a queued refusal stops the rest"
    );
    assert_eq!(
        ran[0].refused.as_ref().expect("refused").reason,
        "the user said no"
    );
}

// --- Deadlines ---

fn deadline_inline_late_fails(make: Make) {
    let bus = make();
    bus.listen::<Trail>(
        "slow",
        ListenOpts::new().deadline(Duration::from_millis(5)),
        |_, v| {
            thread::sleep(Duration::from_millis(30));
            v.push("slow".into());
            Ok(())
        },
    )
    .unwrap();
    push(&*bus, "fast", ListenOpts::new());
    let out = bus.alter::<Trail>((), Vec::new());
    assert_eq!(out.value, ["fast"], "a late edit is dropped");
    assert!(
        matches!(out.failures[0].kind, FailureKind::Late { deadline, .. } if deadline == Duration::from_millis(5)),
        "{:?}",
        out.failures
    );
}

fn deadline_threaded_abandoned(make: Make) {
    let bus = make();
    bus.declare::<Parts>(PointSpec::new("parts").deadline(Duration::from_millis(50)))
        .unwrap();
    bus.listen::<Parts>("stuck", ListenOpts::new().threaded(), |_, out| {
        thread::sleep(Duration::from_secs(3));
        out.push(0);
        Ok(())
    })
    .unwrap();
    bus.listen::<Parts>("quick", ListenOpts::new().threaded(), |n, out| {
        out.push(*n);
        Ok(())
    })
    .unwrap();
    let start = Instant::now();
    let out = bus.collect::<Parts>(4);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "the emit waited for the deadline, not the stuck thread"
    );
    assert_eq!(out.parts, [("quick".to_string(), vec![4])]);
    assert!(matches!(
        out.failures[0].kind,
        FailureKind::Late { took: None, .. }
    ));

    // On a fail-closed request, a missed deadline refuses.
    let bus = make();
    bus.declare::<Run>(PointSpec::new("runs").fail_closed())
        .unwrap();
    bus.listen::<Run>(
        "policy",
        ListenOpts::new()
            .threaded()
            .deadline(Duration::from_millis(20)),
        |_, _| {
            thread::sleep(Duration::from_secs(2));
            Ok(())
        },
    )
    .unwrap();
    bus.listen::<Run>("handler", ListenOpts::new().after("policy"), |_, did| {
        did.push("ran".into());
        Ok(())
    })
    .unwrap();
    let out = bus.alter::<Run>("x".into(), vec![]);
    assert!(out.value.is_empty());
    assert!(out.refused.expect("refused").failed);
}

// --- Introspection ---

/// The address of the object a reference points at, to compare trait
/// objects found by different routes.
fn addr<T: ?Sized>(r: &T) -> *const () {
    r as *const T as *const ()
}

/// Whether the getter and the typed lookup give the same answer.
fn same<C: ?Sized>(getter: Option<&C>, lookup: Option<&C>) -> bool {
    match (getter, lookup) {
        (None, None) => true,
        (Some(g), Some(l)) => addr(g) == addr(l),
        _ => false,
    }
}

fn capability_getter_and_lookup_agree(make: Make) {
    use bus_contract::{Defer, Journal, Relay, Timings};
    let bus = make();
    assert!(
        same(bus.defer(), bus.get::<dyn Defer>()),
        "defer(): the getter and the lookup disagree"
    );
    assert!(
        same(bus.timings(), bus.get::<dyn Timings>()),
        "timings(): the getter and the lookup disagree"
    );
    assert!(
        same(bus.journal(), bus.get::<dyn Journal>()),
        "journal(): the getter and the lookup disagree"
    );
    assert!(
        same(bus.relay(), bus.get::<dyn Relay>()),
        "relay(): the getter and the lookup disagree"
    );
    // `offers` answers from the getters: it sees what the lookup sees.
    assert_eq!(
        offers(&*bus, Capability::Defer),
        bus.get::<dyn Defer>().is_some()
    );
    assert_eq!(
        offers(&*bus, Capability::Timings),
        bus.get::<dyn Timings>().is_some()
    );
    assert_eq!(
        offers(&*bus, Capability::Journal),
        bus.get::<dyn Journal>().is_some()
    );
}

fn introspect_points_listeners_counters(make: Make) {
    let bus = make();
    bus.declare::<Parts>(PointSpec::new("Numbers, one list per listener"))
        .unwrap();
    bus.listen::<Parts>(
        "flaky",
        ListenOpts::new().owner("mod.a").before("other").priority(3),
        |n, _| if *n == 0 { Err("zero".into()) } else { Ok(()) },
    )
    .unwrap();
    push(&*bus, "only", ListenOpts::new());
    bus.collect::<Parts>(1);
    bus.collect::<Parts>(0);
    let points = bus.points();
    let parts = points.iter().find(|p| p.id == Parts::ID).expect("parts");
    assert!(parts.declared);
    assert_eq!(parts.kind, Parts::KIND);
    assert_eq!(parts.description, "Numbers, one list per listener");
    let flaky = &parts.listeners[0];
    assert_eq!(flaky.id, "flaky");
    assert_eq!(flaky.owner.as_deref(), Some("mod.a"));
    assert_eq!(flaky.before, ["other"]);
    assert_eq!(flaky.priority, 3);
    assert_eq!(flaky.missing, ["other"]);
    assert_eq!((flaky.runs, flaky.failures), (2, 1));
    assert!(flaky.last_failure.as_deref().unwrap_or("").contains("zero"));
    let trail = points.iter().find(|p| p.id == Trail::ID).expect("trail");
    assert!(!trail.declared, "listened to, never declared");
    assert!(bus.listeners("nobody").is_none());
}

// --- Defer ---

fn defer_drain_in_ask_order(make: Make) {
    let bus = make();
    bus.declare::<Run>(PointSpec::new("writes")).unwrap();
    let heard: Arc<Mutex<Vec<String>>> = Arc::default();
    let h = heard.clone();
    let again = Arc::downgrade(&bus);
    bus.listen::<Run>("persist", ListenOpts::new(), move |what, did| {
        match what.as_str() {
            "locked" => return Err(HookError::refuse("read-only")),
            "broken" => return Err("disk full".into()),
            _ => {}
        }
        h.lock().unwrap().push(what.clone());
        did.push(format!("wrote {what}"));
        if what == "rename"
            && let Some(bus) = again.upgrade()
        {
            bus.defer()
                .expect("defer")
                .ask::<Run>("reindex".into(), vec![]);
        }
        Ok(())
    })
    .unwrap();
    let defer = bus.defer().expect("defer");
    let first = defer.ask::<Run>("rename".into(), vec![]);
    let second = defer.ask::<Run>("locked".into(), vec![]);
    defer.ask::<Run>("broken".into(), vec![]);
    assert!(second > first, "ask numbers grow");
    assert_eq!(defer.waiting(), 3);
    assert!(heard.lock().unwrap().is_empty(), "an ask runs at the drain");
    bus.alter::<Run>("now".into(), vec![]);
    let ran = defer.drain();
    assert_eq!(ran.len(), 3);
    assert_eq!(ran[0].ask, Some(first));
    assert_eq!(ran[0].ran, ["persist"]);
    assert_eq!(
        ran[1].refused.as_ref().expect("refused").reason,
        "read-only"
    );
    assert_eq!(
        ran[2].failures[0].kind,
        FailureKind::Error("disk full".into())
    );
    assert_eq!(*heard.lock().unwrap(), ["now", "rename"]);
    assert_eq!(defer.waiting(), 1, "an ask made while draining waits");
    assert_eq!(defer.drain().len(), 1);
    assert_eq!(*heard.lock().unwrap(), ["now", "rename", "reindex"]);
    assert!(defer.drain().is_empty());
}

fn defer_asks_per_thread(make: Make) {
    let bus = make();
    let heard: Arc<Mutex<Vec<String>>> = Arc::default();
    let h = heard.clone();
    bus.listen::<Run>("persist", ListenOpts::new(), move |what, _| {
        h.lock().unwrap().push(what.clone());
        Ok(())
    })
    .unwrap();
    bus.defer()
        .expect("defer")
        .ask::<Run>("here".into(), vec![]);
    let other = bus.clone();
    let there = thread::spawn(move || {
        let defer = other.defer().expect("defer");
        defer.ask::<Run>("there".into(), vec![]);
        (defer.waiting(), defer.drain().len())
    })
    .join()
    .unwrap();
    assert_eq!(there, (1, 1), "a thread sees and drains its own asks");
    assert_eq!(*heard.lock().unwrap(), ["there"]);
    assert_eq!(bus.defer().expect("defer").drain().len(), 1);
    assert_eq!(*heard.lock().unwrap(), ["there", "here"]);
}

// --- Timings ---

fn sleeper(bus: &dyn Bus, id: &'static str, ms: u32) {
    bus.listen::<Trail>(id, ListenOpts::new().owner("test"), move |_, v| {
        thread::sleep(MS * ms);
        v.push(id.to_owned());
        Ok(())
    })
    .unwrap();
}

fn timings_timed_until_reset(make: Make) {
    let bus = make();
    let timings = bus.timings().expect("timings");
    sleeper(&*bus, "slow", 4);
    sleeper(&*bus, "quick", 0);
    for _ in 0..3 {
        bus.alter::<Trail>((), Vec::new());
    }
    let t = timings.point(Trail::ID).expect("timed");
    assert_eq!(t.passes.count, 3);
    assert!(t.passes.total >= MS * 12);
    assert!(t.passes.max >= MS * 4);
    let ids: Vec<&str> = t.listeners.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(ids, ["slow", "quick"], "in the order they run");
    assert_eq!(t.listeners[0].stats.count, 3);
    assert_eq!(t.slowest()[0].id, "slow");
    assert!(timings.points().iter().any(|p| p.point == Trail::ID));
    timings.reset();
    assert!(
        timings.point(Trail::ID).is_none(),
        "nothing since the reset"
    );
    bus.alter::<Trail>((), Vec::new());
    assert_eq!(timings.point(Trail::ID).expect("timed").passes.count, 1);
}

fn timings_over_budget_reported(make: Make) {
    let bus = make();
    bus.declare::<Trail>(PointSpec::new("A trail.").budget(MS))
        .unwrap();
    sleeper(&*bus, "quick", 0);
    sleeper(&*bus, "slow", 5);
    let timings = bus.timings().expect("timings");
    let heard: Arc<Mutex<Vec<OverBudget>>> = Arc::default();
    let h = heard.clone();
    timings.on_over_budget(|_| panic!("a broken sink"));
    timings.on_over_budget(move |o| h.lock().unwrap().push(o.clone()));
    let out = bus.alter::<Trail>((), Vec::new());
    assert!(out.failures.is_empty(), "a budget is not a deadline");
    assert_eq!(out.value, ["quick", "slow"], "results kept");
    let heard = heard.lock().unwrap();
    assert_eq!(heard.len(), 1);
    assert_eq!(heard[0].point, Trail::ID);
    assert_eq!(heard[0].budget, MS);
    assert!(heard[0].took >= MS * 5);
    assert_eq!(timings.point(Trail::ID).expect("timed").over_budget, 1);
}

// --- Journal and relay ---

const JOURNALED: &[PointDecl] = &[
    PointDecl::of::<Step>("A step.").journaled(),
    PointDecl::of::<Plain>("Not journaled."),
];

fn journal_refuses_off_journaled(make: Make) {
    let bus = make();
    assert!(declare_all(&*bus, JOURNALED).is_empty());
    assert!(
        matches!(
            bus.declare::<Run>(PointSpec::new("runs").journaled()),
            Err(Refused::NotEvent { .. })
        ),
        "only an event point is journaled"
    );
    let journal = bus.journal().expect("journal");
    let before = journal.last_seq();
    assert!(matches!(
        journal.publish::<Plain>("x".into()),
        Err(Refused::NotJournaled { .. })
    ));
    assert!(matches!(
        journal.publish::<Run>("x".into()),
        Err(Refused::NotJournaled { .. })
    ));
    assert_eq!(journal.last_seq(), before, "nothing recorded");
    let shown: Vec<_> = bus
        .points()
        .iter()
        .filter(|p| p.id == Step::ID || p.id == Plain::ID)
        .map(|p| (p.id, p.journaled))
        .collect();
    assert_eq!(shown, [(Plain::ID, false), (Step::ID, true)]);
}

fn journal_delivered(make: Make) {
    let bus = make();
    assert!(declare_all(&*bus, JOURNALED).is_empty());
    let heard: Arc<Mutex<Vec<(u32, thread::ThreadId)>>> = Arc::default();
    let h = heard.clone();
    bus.listen::<Step>("steps", ListenOpts::new(), move |n, _| {
        h.lock().unwrap().push((*n, thread::current().id()));
        Ok(())
    })
    .unwrap();
    let journal = bus.journal().expect("journal");
    let first = journal.last_seq();
    let seqs: Vec<u64> = (0..5)
        .map(|n| journal.publish::<Step>(n).expect("journaled"))
        .collect();
    assert!(
        seqs.windows(2).all(|w| w[1] > w[0]),
        "numbers grow: {seqs:?}"
    );
    assert!(seqs[0] > first);
    assert_eq!(journal.last_seq(), seqs[4]);
    assert!(journal.flush(WAIT), "delivered within {WAIT:?}");
    let heard = heard.lock().unwrap();
    let order: Vec<u32> = heard.iter().map(|h| h.0).collect();
    assert_eq!(order, [0, 1, 2, 3, 4], "once each, in number order");
    assert!(
        heard.iter().all(|h| h.1 != thread::current().id()),
        "never on the publisher's thread"
    );
    let read = journal.since(seqs[2], 10);
    assert_eq!(read.entries.len(), 2);
    assert_eq!(read.entries[0].read::<Step>(), Some(&3));
}

fn relay_resume_and_ingest(make: Make) {
    let bus = make();
    let table = [PointDecl::of::<Shared>("Shared.").relayable(Codec::of::<Shared>())];
    assert!(declare_all(&*bus, &table).is_empty());
    assert!(
        bus.points()
            .iter()
            .any(|p| p.id == Shared::ID && p.relayable && p.journaled)
    );
    let journal = bus.journal().expect("journal");
    let relay = bus.relay().expect("relay");
    let from = relay.cursor();
    journal.publish::<Shared>(41).expect("journaled");
    journal.publish::<Shared>(42).expect("journaled");
    assert!(journal.flush(WAIT));
    let read = relay.resume(from, 1);
    assert_eq!(read.entries.len(), 1, "in pages");
    assert_eq!(read.entries[0].kind, Shared::ID);
    assert_eq!(read.entries[0].text, "41");
    let rest = relay.resume(read.next, 10);
    let texts: Vec<_> = rest.entries.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(texts, ["42"], "resumed from the cursor");
    assert!(rest.lost.is_none());
    assert_ne!(rest.next, Cursor::START);

    let peer = Origin::process("peer");
    assert!(matches!(
        relay.ingest(&peer, RelayedEntry::new(1, Shared::ID, "not a number")),
        Err(IngestError::Decode { .. })
    ));
    bus.declare::<Plain>(PointSpec::new("Not relayable."))
        .unwrap();
    assert!(matches!(
        relay.ingest(&peer, RelayedEntry::new(2, Plain::ID, "x")),
        Err(IngestError::NotRelayable { .. })
    ));
    let heard: Arc<Mutex<Vec<u32>>> = Arc::default();
    let h = heard.clone();
    bus.listen::<Shared>("shared", ListenOpts::new(), move |n, _| {
        h.lock().unwrap().push(*n);
        Ok(())
    })
    .unwrap();
    let seq = relay
        .ingest(&peer, RelayedEntry::new(3, Shared::ID, "7"))
        .expect("ingested");
    assert!(journal.flush(WAIT));
    assert_eq!(*heard.lock().unwrap(), [7], "taken in as a local entry");
    let kept = journal.since(seq - 1, 1).entries;
    assert_eq!(kept[0].origin, Some(peer), "kept with its origin");
}

// --- Crossing: requests answered on the other side ---

/// A request one bus answers for another; the value is what each side
/// did.
struct Delete;
impl Point for Delete {
    const ID: &'static str = "conformance.delete";
    const KIND: PointKind = PointKind::Request;
    type Input = String;
    type Value = Vec<String>;
}

impl Relayed for Delete {
    fn encode(input: &String) -> String {
        input.clone()
    }
    fn decode(text: &str) -> Result<String, String> {
        Ok(text.to_owned())
    }
}

/// `a,b,c`; a `|` cannot be read.
impl RelayedRequest for Delete {
    fn encode_value(value: &Vec<String>) -> String {
        value.join(",")
    }
    fn decode_value(text: &str) -> Result<Vec<String>, String> {
        if text.contains('|') {
            return Err("a `|` in the value".into());
        }
        Ok(text
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect())
    }
}

/// An alter point: it never crosses.
struct Title;
impl Point for Title {
    const ID: &'static str = "conformance.title";
    type Input = ();
    type Value = String;
}

impl Relayed for Title {
    fn encode(_: &()) -> String {
        String::new()
    }
    fn decode(_: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Wide: an ask meant to be answered in time, on a loaded machine too.
const ASK: Duration = Duration::from_secs(5);

fn crossing_table(version: u32) -> [PointDecl; 2] {
    [
        PointDecl::of::<Shared>("Shared.")
            .relayable(Codec::of::<Shared>())
            .version(version),
        PointDecl::of::<Delete>("Deletes.")
            .relayable(Codec::request::<Delete>())
            .version(version),
    ]
}

fn asker() -> Origin {
    Origin::process("asker")
}

fn crossing_bus(make: Make, version: u32) -> Arc<dyn Bus> {
    let bus = make();
    assert!(declare_all(&*bus, &crossing_table(version)).is_empty());
    bus
}

/// An asker with a listener of its own before the far side's turn, and
/// the far bus at `far_version`, linked by a direct call.
fn asker_and_far(make: Make, far_version: u32) -> (Arc<dyn Bus>, Arc<dyn Bus>) {
    let far = crossing_bus(make, far_version);
    let here = crossing_bus(make, 1);
    here.listen::<Delete>("here.confirm", ListenOpts::new(), |_, did| {
        did.push("here.confirm".into());
        Ok(())
    })
    .unwrap();
    let link = far.clone();
    here.listen_any(
        PointKey::of::<Delete>(),
        "here.far".into(),
        ListenOpts::new().deadline(ASK).owner("bridge"),
        far_listener(Codec::request::<Delete>(), ASK, 1, move |ask| {
            Some(link.relay().unwrap().answer(&asker(), ask))
        }),
    )
    .unwrap();
    (here, far)
}

/// The far side's listener; what it was asked.
fn far_deletes(far: &Arc<dyn Bus>) -> Arc<Mutex<Vec<String>>> {
    let ran: Arc<Mutex<Vec<String>>> = Arc::default();
    let r = ran.clone();
    far.listen::<Delete>("far.vault", ListenOpts::new(), move |path, did| {
        r.lock().unwrap().push(path.clone());
        did.push("far.vault".into());
        Ok(())
    })
    .unwrap();
    ran
}

fn ask(run: u64, id: u64, kind: &str, deadline: Duration, input: &str) -> RelayedAsk {
    RelayedAsk::new(run, id, kind, deadline, input, "")
}

fn crossing_declared(make: Make) {
    let bus = crossing_bus(make, 3);
    let info = bus
        .points()
        .into_iter()
        .find(|p| p.id == Delete::ID)
        .expect("declared");
    assert!(info.relayable, "a request crosses");
    assert!(info.fail_closed, "a relayable request is fail-closed");
    assert!(!info.journaled, "a request is not journaled");
    assert_eq!(info.version, 3);

    let bus = make();
    let table = [
        PointDecl::of::<Title>("A title.").relayable(Codec::of::<Title>()),
        PointDecl::of::<Delete>("No value codec.").relayable(Codec::of::<Delete>()),
    ];
    assert_eq!(
        declare_all(&*bus, &table),
        [
            Refused::NotCrossable { point: Title::ID },
            Refused::NotCrossable { point: Delete::ID },
        ]
    );
    assert!(bus.points().iter().all(|p| !p.declared), "nothing declared");
    assert_eq!(
        bus.declare::<Title>(PointSpec::new("A title.").relayable_request(Codec::of::<Title>())),
        Err(Refused::NotCrossable { point: Title::ID }),
        "declared one by one too"
    );
}

fn crossing_answered_far_means_the_same(make: Make) {
    let (here, far) = asker_and_far(make, 1);
    let ran = far_deletes(&far);
    let out = here.alter::<Delete>("/a.jpg".into(), Vec::new());
    assert!(out.refused.is_none(), "{:?}", out.refused);
    assert_eq!(out.value, ["here.confirm", "far.vault"]);
    assert_eq!(*ran.lock().unwrap(), ["/a.jpg"]);
    // The same request emitted on the far bus itself: the same value.
    let local = far.alter::<Delete>("/a.jpg".into(), vec!["here.confirm".into()]);
    assert_eq!(local.value, out.value);
}

fn crossing_veto_reason(make: Make) {
    let (here, far) = asker_and_far(make, 1);
    far.listen::<Delete>("far.guard", ListenOpts::new(), |_, _| {
        Err(HookError::refuse("the volume is read-only"))
    })
    .unwrap();
    let ran = far_deletes(&far);
    let out = here.alter::<Delete>("/a.jpg".into(), Vec::new());
    let refusal = out.refused.expect("refused");
    assert_eq!(refusal.listener, "here.far");
    assert!(!refusal.failed, "a veto, not a failure");
    assert!(
        refusal.reason.contains("far.guard") && refusal.reason.contains("read-only"),
        "{}",
        refusal.reason
    );
    assert!(
        ran.lock().unwrap().is_empty(),
        "the veto stopped the far side"
    );
    assert_eq!(out.value, ["here.confirm"], "the far edit is dropped");
}

fn crossing_no_answer(make: Make) {
    type Reply = fn(&RelayedAsk) -> Option<RelayedAnswer>;
    let cases: [(&str, Duration, Reply); 4] = [
        ("no answer", ASK, |_| None),
        ("a late answer", Duration::from_millis(20), |ask| {
            thread::sleep(Duration::from_millis(80));
            Some(RelayedAnswer::to(ask, Answer::Done { value: "x".into() }))
        }),
        ("an answer to another ask", ASK, |ask| {
            let mut other = ask.clone();
            other.id += 1;
            Some(RelayedAnswer::to(
                &other,
                Answer::Done { value: "x".into() },
            ))
        }),
        ("an unreadable answer", ASK, |ask| {
            Some(RelayedAnswer::to(
                ask,
                Answer::Done {
                    value: "a|b".into(),
                },
            ))
        }),
    ];
    for (case, deadline, reply) in cases {
        let bus = crossing_bus(make, 1);
        bus.listen_any(
            PointKey::of::<Delete>(),
            "here.odd".into(),
            ListenOpts::new().deadline(WAIT),
            far_listener(Codec::request::<Delete>(), deadline, 1, move |ask| {
                reply(&ask)
            }),
        )
        .unwrap();
        let out = bus.alter::<Delete>("/a.jpg".into(), Vec::new());
        assert!(out.refused.is_some(), "{case} refuses");
        assert!(out.value.is_empty(), "{case}: nothing kept");
    }
}

fn crossing_far_deadline(make: Make) {
    let far = crossing_bus(make, 1);
    let ran = far_deletes(&far);
    let relay = far.relay().unwrap();
    let late = ask(1, 1, Delete::ID, Duration::ZERO, "/a");
    thread::sleep(MS);
    match relay.answer(&asker(), late).answer {
        Answer::Refused { listener: None, .. } => {}
        other => panic!("an ask past its deadline runs nothing: {other:?}"),
    }
    assert!(ran.lock().unwrap().is_empty(), "no listener ran");

    // Listeners that end past the deadline: refused (failed), whatever
    // they did, as the asker holds it.
    far.listen::<Delete>("far.slow", ListenOpts::new().deadline(WAIT), |_, _| {
        thread::sleep(Duration::from_millis(60));
        Ok(())
    })
    .unwrap();
    let slow = ask(1, 2, Delete::ID, Duration::from_millis(10), "/b");
    match relay.answer(&asker(), slow).answer {
        Answer::Refused { failed: true, .. } => {}
        other => panic!("listeners ended past the deadline: {other:?}"),
    }
}

fn crossing_answered_once(make: Make) {
    let far = crossing_bus(make, 1);
    let ran = far_deletes(&far);
    let relay = far.relay().unwrap();
    let first_ask = ask(7, 1, Delete::ID, ASK, "/a");
    let first = relay.answer(&asker(), first_ask.clone());
    let again = relay.answer(&asker(), first_ask.clone());
    assert_eq!(first, again, "a repeat gets the first answer");
    assert_eq!(
        first.answer,
        Answer::Done {
            value: "far.vault".into()
        }
    );
    assert_eq!(ran.lock().unwrap().len(), 1, "run once");
    // Another run (the asker restarted) is another ask; so is the same
    // number from another process.
    let mut restarted = first_ask.clone();
    restarted.run = 8;
    relay.answer(&asker(), restarted);
    assert_eq!(ran.lock().unwrap().len(), 2);
    relay.answer(&Origin::process("other"), first_ask);
    assert_eq!(ran.lock().unwrap().len(), 3);
}

fn crossing_not_relayable_refused(make: Make) {
    let far = crossing_bus(make, 1);
    far.declare::<Run>(PointSpec::new("Local only.")).unwrap();
    let relay = far.relay().unwrap();
    for (n, kind) in ["conformance.unknown", Run::ID, Shared::ID]
        .into_iter()
        .enumerate()
    {
        match relay
            .answer(&asker(), ask(1, n as u64, kind, ASK, "1"))
            .answer
        {
            Answer::Refused { listener: None, .. } => {}
            other => panic!("{kind}: {other:?}"),
        }
    }
    // An input or a value it cannot read.
    let mut odd = ask(1, 10, Delete::ID, ASK, "/a");
    odd.value = "a|b".into();
    assert!(matches!(
        relay.answer(&asker(), odd).answer,
        Answer::Refused { listener: None, .. }
    ));
    // A request is asked, not ingested.
    assert_eq!(
        relay.ingest(&asker(), RelayedEntry::new(1, Delete::ID, "/a")),
        Err(IngestError::NotRelayable {
            kind: Delete::ID.into()
        })
    );
}

fn crossing_origin_kept(make: Make) {
    let bus = crossing_bus(make, 1);
    let journal = bus.journal().unwrap();
    let from = Origin::process("agent-socket").agent("claude");
    let seq = bus
        .relay()
        .unwrap()
        .ingest(&from, RelayedEntry::new(1, Shared::ID, "5"))
        .unwrap();
    let unknown = bus
        .relay()
        .unwrap()
        .ingest(&from, RelayedEntry::new(2, "conformance.unknown", "{}"))
        .unwrap();
    let kept = journal.since(seq - 1, 10).entries;
    assert_eq!(kept.len(), 2);
    assert_eq!(kept[0].origin, Some(from.clone()));
    assert_eq!(kept[1].seq, unknown);
    assert_eq!(kept[1].origin, Some(from), "an unknown kind too");
    // Taken in, not served again.
    assert!(
        bus.relay()
            .unwrap()
            .resume(Cursor::START, 10)
            .entries
            .is_empty()
    );
}

fn crossing_authority_asks(make: Make) {
    let far = crossing_bus(make, 1);
    let ran = far_deletes(&far);
    let relay = far.relay().unwrap();
    // What each crossing showed: origin, kind, version, ask.
    type Seen = Vec<(String, String, u32, bool)>;
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let s = seen.clone();
    relay.authorize(move |c| {
        s.lock()
            .unwrap()
            .push((c.origin.to_string(), c.kind.to_owned(), c.version, c.ask));
        Ok(())
    });
    relay.authorize(|c| match &c.origin.agent {
        Some(agent) if c.kind == Delete::ID => Err(format!("{agent} may not delete")),
        _ => Ok(()),
    });
    let agent = asker().agent("claude");
    assert_eq!(
        relay
            .answer(&agent, ask(1, 1, Delete::ID, ASK, "/a"))
            .answer,
        Answer::refused("claude may not delete")
    );
    assert!(ran.lock().unwrap().is_empty(), "nothing ran");
    // Checked before anything is read: an unreadable ask is refused by
    // the authority, not the codec.
    let mut odd = ask(1, 2, Delete::ID, ASK, "/a");
    odd.value = "a|b".into();
    assert_eq!(
        relay.answer(&agent, odd).answer,
        Answer::refused("claude may not delete")
    );
    // The process itself is let through.
    assert!(matches!(
        relay
            .answer(&asker(), ask(1, 3, Delete::ID, ASK, "/a"))
            .answer,
        Answer::Done { .. }
    ));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 3, "every ask shown, in order");
    assert_eq!(
        seen[0],
        ("asker (agent claude)".into(), Delete::ID.into(), 1, true)
    );
}

fn crossing_authority_events(make: Make) {
    let bus = crossing_bus(make, 1);
    let journal = bus.journal().unwrap();
    let relay = bus.relay().unwrap();
    let heard: Arc<Mutex<Vec<u32>>> = Arc::default();
    let h = heard.clone();
    bus.listen::<Shared>("shared", ListenOpts::new(), move |n, _| {
        h.lock().unwrap().push(*n);
        Ok(())
    })
    .unwrap();
    relay.authorize(|c| {
        assert!(!c.ask, "an event");
        match c.origin.process.as_str() {
            "rogue" => panic!("an authority panics"),
            "banned" => Err("banned".into()),
            _ => Ok(()),
        }
    });
    let before = journal.last_seq();
    for from in ["banned", "rogue"] {
        let err = relay
            .ingest(
                &Origin::process(from),
                RelayedEntry::new(1, Shared::ID, "not even a number"),
            )
            .unwrap_err();
        assert!(matches!(err, IngestError::Refused { .. }), "{from}: {err}");
    }
    assert_eq!(journal.last_seq(), before, "nothing recorded");
    relay
        .ingest(&asker(), RelayedEntry::new(2, Shared::ID, "9"))
        .expect("let through");
    assert!(journal.flush(WAIT));
    assert_eq!(*heard.lock().unwrap(), [9], "only what was let through");
}

fn crossing_same_version(make: Make) {
    let (here, far) = asker_and_far(make, 2);
    let ran = far_deletes(&far);
    let out = here.alter::<Delete>("/a.jpg".into(), Vec::new());
    let refusal = out.refused.expect("another version refuses");
    assert!(refusal.reason.contains("version"), "{}", refusal.reason);
    assert!(ran.lock().unwrap().is_empty());

    let relay = far.relay().unwrap();
    let before = far.journal().unwrap().last_seq();
    assert_eq!(
        relay.ingest(&asker(), RelayedEntry::new(1, Shared::ID, "1")),
        Err(IngestError::Version {
            kind: Shared::ID.into(),
            sent: 1,
            here: 2
        })
    );
    assert_eq!(
        far.journal().unwrap().last_seq(),
        before,
        "nothing recorded"
    );

    // Served with its version.
    let from = relay.cursor();
    far.journal().unwrap().publish::<Shared>(3).unwrap();
    let read = relay.resume(from, 10);
    assert_eq!(read.entries.len(), 1);
    assert_eq!(read.entries[0].version, 2);
}

// --- Core: checks for the guarantees page's open rows ---

/// A listener that listens and unlistens on the point it is run by.
/// Whether the change shows in that same emit is not specified; it must
/// not block, and the next emit sees it.
fn listen_on_the_emitting_point(make: Make) {
    let bus = make();
    push(&*bus, "doomed", ListenOpts::new());
    let inner = Arc::downgrade(&bus);
    bus.listen::<Trail>(
        "changer",
        ListenOpts::new().before("doomed"),
        move |_, v| {
            let bus = inner.upgrade().expect("the bus is alive");
            let known = bus.listeners(Trail::ID).unwrap_or_default();
            if !known.iter().any(|l| l.id == "late") {
                bus.listen::<Trail>("late", ListenOpts::new(), |_, v| {
                    v.push("late".into());
                    Ok(())
                })
                .map_err(|e| HookError::from(e.to_string()))?;
            }
            bus.unlisten(Trail::ID, "doomed");
            v.push("changer".into());
            Ok(())
        },
    )
    .unwrap();
    let (tx, rx) = mpsc::channel();
    let emitter = bus.clone();
    thread::spawn(move || {
        let _ = tx.send(emitter.alter::<Trail>((), Vec::new()));
    });
    let out = match rx.recv_timeout(WAIT) {
        Ok(out) => out,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("listening on the emitting point blocked the emit for {WAIT:?}")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the emit panicked"),
    };
    assert!(out.failures.is_empty(), "{:?}", out.failures);
    assert_eq!(out.value.first().map(String::as_str), Some("changer"));
    let out = bus.alter::<Trail>((), Vec::new());
    assert_eq!(
        out.value,
        ["changer", "late"],
        "the next emit runs the listener added and not the one removed"
    );
}

fn refusal_keeps_earlier_effects(make: Make) {
    for fail_closed in [false, true] {
        let bus = make();
        if fail_closed {
            bus.declare::<Run>(PointSpec::new("runs").fail_closed())
                .unwrap();
        }
        let effects = Arc::new(Mutex::new(0));
        let e = effects.clone();
        bus.listen::<Run>("first", ListenOpts::new(), move |_, did| {
            *e.lock().unwrap() += 1;
            did.push("first".into());
            Ok(())
        })
        .unwrap();
        bus.listen::<Run>("stop", ListenOpts::new().after("first"), move |_, did| {
            did.push("stop".into());
            if fail_closed {
                Err("broken".into())
            } else {
                Err(HookError::refuse("no"))
            }
        })
        .unwrap();
        bus.listen::<Run>("never", ListenOpts::new().after("stop"), |_, did| {
            did.push("never".into());
            Ok(())
        })
        .unwrap();
        let out = bus.alter::<Run>("x".into(), vec![]);
        let refusal = out.refused.expect("refused");
        assert_eq!(refusal.failed, fail_closed);
        assert_eq!(
            out.value,
            ["first"],
            "the value as the listeners before the stop left it"
        );
        assert_eq!(*effects.lock().unwrap(), 1, "their effects stay");
    }
}

fn local_queued_emits_run_in_order(make: Make) {
    let bus = make();
    let heard: Rc<RefCell<Vec<String>>> = Rc::default();
    let h = heard.clone();
    bus.listen_local::<Parts>("ui.parts", ListenOpts::new(), move |n, _| {
        h.borrow_mut().push(format!("parts {n}"));
        Ok(())
    })
    .unwrap();
    let h = heard.clone();
    bus.listen_local::<Trail>("ui.trail", ListenOpts::new(), move |_, _| {
        h.borrow_mut().push("trail".into());
        Ok(())
    })
    .unwrap();
    let other = bus.clone();
    thread::spawn(move || {
        other.collect::<Parts>(1);
        other.alter::<Trail>((), Vec::new());
        other.collect::<Parts>(2);
    })
    .join()
    .unwrap();
    assert!(heard.borrow().is_empty(), "queued, not run");
    assert_eq!(bus.run_pending().len(), 3);
    assert_eq!(
        *heard.borrow(),
        ["parts 1", "trail", "parts 2"],
        "emits run in the order they were queued"
    );
}

fn local_panicking_waker_ignored(make: Make) {
    let bus = make();
    bus.on_pending(|_| panic!("a broken waker"));
    let woken = Arc::new(Mutex::new(Vec::new()));
    let w = woken.clone();
    bus.on_pending(move |t| w.lock().unwrap().push(t));
    bus.listen_local::<Trail>("ui", ListenOpts::new(), |_, v| {
        v.push("ui".into());
        Ok(())
    })
    .unwrap();
    push(&*bus, "core", ListenOpts::new());
    let other = bus.clone();
    let out = thread::spawn(move || other.alter::<Trail>((), Vec::new()))
        .join()
        .expect("a panicking waker does not reach the emitter");
    assert_eq!(out.value, ["core"]);
    assert_eq!(out.deferred, ["ui"]);
    assert_eq!(
        *woken.lock().unwrap(),
        [thread::current().id()],
        "the other wakers are called"
    );
    let ran = bus.run_pending();
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0].ran, ["ui"]);
}

// --- Defer: checks for the guarantees page's open rows ---

fn run_listener(bus: &dyn Bus, id: &'static str) {
    bus.listen::<Run>(id, ListenOpts::new(), move |_, did| {
        did.push(id.into());
        Ok(())
    })
    .unwrap();
}

fn defer_ask_wakes_its_thread(make: Make) {
    let bus = make();
    run_listener(&*bus, "persist");
    bus.on_pending(|_| panic!("a broken waker"));
    let woken: Arc<Mutex<Vec<ThreadId>>> = Arc::default();
    let w = woken.clone();
    bus.on_pending(move |t| w.lock().unwrap().push(t));
    let other = bus.clone();
    let (asker, drained) = thread::spawn(move || {
        let defer = other.defer().expect("defer");
        defer.ask::<Run>("x".into(), vec![]);
        (thread::current().id(), defer.drain())
    })
    .join()
    .expect("a panicking waker does not reach the asker");
    assert_eq!(
        *woken.lock().unwrap(),
        [asker],
        "an ask calls the wakers with its thread's id"
    );
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].ran, ["persist"]);
}

fn defer_listeners_read_at_the_drain(make: Make) {
    let bus = make();
    run_listener(&*bus, "gone");
    run_listener(&*bus, "kept");
    let defer = bus.defer().expect("defer");
    defer.ask::<Run>("x".into(), vec![]);
    assert!(bus.unlisten(Run::ID, "gone"));
    run_listener(&*bus, "added");
    let ran = defer.drain();
    assert_eq!(ran.len(), 1);
    assert_eq!(
        ran[0].ran,
        ["kept", "added"],
        "a listener removed after the ask does not run, one added does"
    );
}

// --- Timings: checks for the guarantees page's open rows ---

fn timings_threaded_late_counts_the_wait(make: Make) {
    let bus = make();
    let timings = bus.timings().expect("timings");
    let deadline = MS * 30;
    bus.listen::<Parts>(
        "stuck",
        ListenOpts::new().threaded().deadline(deadline),
        |_, _| {
            thread::sleep(Duration::from_secs(2));
            Ok(())
        },
    )
    .unwrap();
    let out = bus.collect::<Parts>(1);
    assert!(matches!(
        out.failures[0].kind,
        FailureKind::Late { took: None, .. }
    ));
    let t = timings.point(Parts::ID).expect("timed");
    assert_eq!(t.passes.count, 1);
    let stuck = t
        .listeners
        .iter()
        .find(|l| l.id == "stuck")
        .expect("a listener that missed its deadline is timed");
    assert_eq!(stuck.stats.count, 1, "timed when the emit stopped waiting");
    assert!(
        stuck.stats.total >= deadline,
        "it counts the time the emit waited: {:?}",
        stuck.stats.total
    );
}

// --- Journal: checks for the guarantees page's open rows ---

/// How many entries a check publishes at most to push an entry out of a
/// journal's window. A journal that keeps more is not pushed that far:
/// the check's gap part passes without being exercised on it.
pub const WINDOW_PROBE: u32 = 1 << 14;

/// Calls `publish` until the entry `seq` has left the window,
/// up to [`WINDOW_PROBE`]; whether it left.
fn push_out(journal: &dyn bus_contract::Journal, seq: u64, publish: impl Fn(u32)) -> bool {
    let kept = || journal.since(seq - 1, 1).entries.first().map(|e| e.seq) == Some(seq);
    let mut n = 0;
    while kept() {
        if n == WINDOW_PROBE {
            return false;
        }
        publish(n);
        n += 1;
    }
    true
}

fn journal_numbers_grow_by_one(make: Make) {
    let bus = make();
    assert!(declare_all(&*bus, JOURNALED).is_empty());
    let journal = bus.journal().expect("journal");
    let before = journal.last_seq();
    let seqs: Vec<u64> = (0..5)
        .map(|n| journal.publish::<Step>(n).expect("journaled"))
        .collect();
    let expected: Vec<u64> = (before + 1..=before + 5).collect();
    assert_eq!(seqs, expected, "from 1, by one");
    assert_eq!(journal.last_seq(), before + 5);
}

fn journal_since_reports_the_gap(make: Make) {
    let bus = make();
    assert!(declare_all(&*bus, JOURNALED).is_empty());
    let journal = bus.journal().expect("journal");
    let first = journal.publish::<Step>(0).expect("journaled");
    let read = journal.since(first - 1, 10);
    assert!(!read.gap, "nothing dropped yet");
    assert_eq!(read.entries.first().map(|e| e.seq), Some(first));
    assert_eq!(read.last_seq, first);
    let pushed = push_out(journal, first, |n| {
        journal.publish::<Step>(n).expect("journaled");
    });
    if !pushed {
        return;
    }
    let read = journal.since(first - 1, 10);
    assert!(read.gap, "entries after {} left the window", first - 1);
    assert!(read.entries.first().is_some_and(|e| e.seq > first));
    let last = journal.last_seq();
    let read = journal.since(last - 1, 10);
    assert!(!read.gap, "a read inside the window has no gap");
    assert_eq!(read.entries.len(), 1);
}

fn journal_wait_and_wait_for(make: Make) {
    let bus = make();
    assert!(declare_all(&*bus, JOURNALED).is_empty());
    let journal = bus.journal().expect("journal");
    let before = journal.last_seq();
    let seq = journal.publish::<Step>(7).expect("journaled");
    let start = Instant::now();
    let got = journal.wait(&[Step::ID], before, WAIT).expect("there");
    assert!(
        start.elapsed() < WAIT / 2,
        "an entry already there is returned at once"
    );
    assert_eq!(got.seq, seq);
    assert_eq!(got.read::<Step>(), Some(&7));
    assert_eq!(
        journal.wait(&[], before, WAIT).map(|e| e.seq),
        Some(seq),
        "no pattern matches every kind"
    );
    let start = Instant::now();
    assert!(
        journal
            .wait(&["conformance.nothing"], before, MS * 50)
            .is_none()
    );
    assert!(start.elapsed() >= MS * 50, "it waits up to the timeout");
    let later = bus.clone();
    let publisher = thread::spawn(move || {
        thread::sleep(MS * 50);
        later.journal().unwrap().publish::<Step>(8).unwrap()
    });
    let got = journal.wait_for::<Step>(seq, WAIT);
    let published = publisher.join().unwrap();
    assert_eq!(got, Some((published, 8)), "an entry to come is waited for");
}

fn journal_cause_kept(make: Make) {
    let bus = make();
    assert!(declare_all(&*bus, JOURNALED).is_empty());
    let journal = bus.journal().expect("journal");
    let cause = journal.publish::<Step>(1).expect("journaled");
    let caused = journal.publish_caused::<Step>(2, cause).expect("journaled");
    let read = journal.since(cause - 1, 10);
    let found = |seq| {
        read.entries
            .iter()
            .find(|e| e.seq == seq)
            .map(|e| e.cause)
            .expect("kept")
    };
    assert_eq!(found(cause), None);
    assert_eq!(found(caused), Some(cause), "the cause is kept");
}

// --- Crossing: checks for the guarantees page's open rows ---

fn crossing_relay_implies_journal(make: Make) {
    let bus = make();
    if bus.relay().is_some() {
        assert!(bus.journal().is_some(), "a bus with Relay offers Journal");
        assert!(offers(&*bus, Capability::Relay));
    }
}

fn texts(read: &bus_contract::Resumed) -> Vec<&str> {
    read.entries.iter().map(|e| e.text.as_str()).collect()
}

fn crossing_lost_entries_reported(make: Make) {
    let bus = crossing_bus(make, 1);
    let journal = bus.journal().expect("journal");
    let relay = bus.relay().expect("relay");
    let first = journal.publish::<Shared>(1).expect("journaled");
    journal.publish::<Shared>(2).expect("journaled");
    let all = relay.resume(Cursor::START, 10);
    assert_eq!(all.lost, None, "the start loses nothing");
    assert_eq!(texts(&all), ["1", "2"]);

    let here = relay.cursor();
    let other = Cursor {
        run: if here.run == u64::MAX {
            1
        } else {
            here.run + 1
        },
        seq: 1,
    };
    let read = relay.resume(other, 10);
    assert_eq!(read.lost, Some(Lost::Restarted), "a cursor of another run");
    assert_eq!(texts(&read), ["1", "2"], "the run's entries from its start");
    assert_eq!(read.next.run, here.run);

    let after_first = Cursor {
        run: here.run,
        seq: first,
    };
    assert_eq!(relay.resume(after_first, 10).lost, None, "nothing left yet");
    let pushed = push_out(journal, first + 1, |n| {
        journal.publish::<Shared>(n).expect("journaled");
    });
    if !pushed {
        return;
    }
    let read = relay.resume(after_first, 1);
    assert_eq!(
        read.lost,
        Some(Lost::Gap),
        "the next entries left the window"
    );
    assert!(read.entries.first().is_some_and(|e| e.seq > first + 1));
}

fn crossing_ingested_not_echoed(make: Make) {
    let bus = crossing_bus(make, 1);
    let journal = bus.journal().expect("journal");
    let relay = bus.relay().expect("relay");
    let from = relay.cursor();
    let peer = Origin::process("peer");
    relay
        .ingest(&peer, RelayedEntry::new(1, Shared::ID, "5"))
        .expect("ingested");
    relay
        .ingest(&peer, RelayedEntry::new(2, "elsewhere.happened", "x"))
        .expect("kept");
    journal.publish::<Shared>(6).expect("journaled");
    assert!(journal.flush(WAIT));
    assert_eq!(
        texts(&relay.resume(from, 10)),
        ["6"],
        "entries taken in are not served again"
    );
    assert_eq!(texts(&relay.resume(Cursor::START, 10)), ["6"]);
}
