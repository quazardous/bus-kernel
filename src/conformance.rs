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
//! Each check builds the buses it needs with `make`, fresh ones, so a
//! check never sees another's points. A check needing a capability the
//! bus does not offer ([`Capability`]) is skipped, not failed. Times are
//! checked as lower bounds or with wide margins: a loaded machine makes
//! runs longer, never shorter.

use std::cell::RefCell;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use web_time::Instant;

use bus_contract::{
    Bus, Capability, Codec, Cursor, FailureKind, HookError, IngestError, LAST, ListenOpts,
    OverBudget, Point, PointDecl, PointKind, PointSpec, Refused, Relayed, RelayedEntry,
    declare_all, offers, order_dump,
};

/// Builds a fresh bus for a check.
pub type Make = fn() -> Arc<dyn Bus>;

/// One check of the suite.
#[derive(Clone, Copy)]
pub struct Check {
    /// Its name, stable: `group.what`.
    pub name: &'static str,
    /// The capabilities it needs; without one, it is skipped.
    pub needs: &'static [Capability],
    body: fn(Make),
}

impl fmt::Debug for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Check")
            .field("name", &self.name)
            .field("needs", &self.needs)
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

/// Every check, by group.
pub fn checks() -> &'static [Check] {
    CHECKS
}

/// Runs every check and returns each one's outcome, in order.
pub fn report(make: Make) -> Vec<(&'static str, Outcome)> {
    checks().iter().map(|c| (c.name, c.run(make))).collect()
}

/// Runs every check; panics naming each failed one and why.
pub fn run(make: Make) {
    let failed: Vec<String> = report(make)
        .into_iter()
        .filter_map(|(name, outcome)| match outcome {
            Outcome::Failed(why) => Some(format!("- {name}: {why}")),
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

const fn check(name: &'static str, body: fn(Make)) -> Check {
    Check {
        name,
        needs: &[],
        body,
    }
}

const fn needs(name: &'static str, needs: &'static [Capability], body: fn(Make)) -> Check {
    Check { name, needs, body }
}

const CHECKS: &[Check] = &[
    check("declare.once_and_typed", declare_once_and_typed),
    check(
        "declare.missing_capability_refused",
        missing_capability_refused,
    ),
    check(
        "order.constraints_priority_registration",
        order_constraints_priority_registration,
    ),
    check(
        "order.cycle_refused_unchanged",
        order_cycle_refused_unchanged,
    ),
    check(
        "order.missing_target_reported_ignored",
        order_missing_target_reported_ignored,
    ),
    check(
        "order.last_runs_after_all_alone",
        order_last_runs_after_all_alone,
    ),
    check(
        "emit.alter_keeps_successful_edits",
        alter_keeps_successful_edits,
    ),
    check(
        "emit.collect_gathers_fresh_parts",
        collect_gathers_fresh_parts,
    ),
    check(
        "emit.failure_sinks_hear_dropped_only",
        failure_sinks_hear_dropped_only,
    ),
    check(
        "emit.reentrant_and_cross_thread",
        reentrant_and_cross_thread,
    ),
    check(
        "request.refusal_stops_not_a_failure",
        refusal_stops_not_a_failure,
    ),
    check(
        "request.refusal_off_a_request_fails",
        refusal_off_a_request_fails,
    ),
    check("request.fail_closed_refuses", fail_closed_refuses),
    check("remove.unlisten_and_forget", unlisten_and_forget),
    check("local.ordered_and_queued", local_ordered_and_queued),
    check("local.error_boundary", local_error_boundary),
    check("local.forget_from_any_thread", local_forget_from_any_thread),
    check("local.deferred_collect_fresh", local_deferred_collect_fresh),
    check(
        "local.refused_request_queues_nothing",
        local_refused_request_queues_nothing,
    ),
    check("deadline.inline_late_fails", deadline_inline_late_fails),
    check("deadline.threaded_abandoned", deadline_threaded_abandoned),
    check(
        "introspect.points_listeners_counters",
        introspect_points_listeners_counters,
    ),
    needs(
        "defer.drain_in_ask_order",
        &[Capability::Defer],
        defer_drain_in_ask_order,
    ),
    needs(
        "defer.asks_per_thread",
        &[Capability::Defer],
        defer_asks_per_thread,
    ),
    needs(
        "timings.timed_until_reset",
        &[Capability::Timings],
        timings_timed_until_reset,
    ),
    needs(
        "timings.over_budget_reported",
        &[Capability::Timings],
        timings_over_budget_reported,
    ),
    needs(
        "journal.refuses_off_journaled",
        &[Capability::Journal],
        journal_refuses_off_journaled,
    ),
    needs(
        "journal.delivered_in_order_off_publisher",
        &[Capability::Journal],
        journal_delivered,
    ),
    needs(
        "relay.resume_and_ingest",
        &[Capability::Journal, Capability::Relay],
        relay_resume_and_ingest,
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

    assert!(matches!(
        relay.ingest("peer", RelayedEntry::new(1, Shared::ID, "not a number")),
        Err(IngestError::Decode { .. })
    ));
    bus.declare::<Plain>(PointSpec::new("Not relayable."))
        .unwrap();
    assert!(matches!(
        relay.ingest("peer", RelayedEntry::new(2, Plain::ID, "x")),
        Err(IngestError::NotRelayable { .. })
    ));
    let heard: Arc<Mutex<Vec<u32>>> = Arc::default();
    let h = heard.clone();
    bus.listen::<Shared>("shared", ListenOpts::new(), move |n, _| {
        h.lock().unwrap().push(*n);
        Ok(())
    })
    .unwrap();
    relay
        .ingest("peer", RelayedEntry::new(3, Shared::ID, "7"))
        .expect("ingested");
    assert!(journal.flush(WAIT));
    assert_eq!(*heard.lock().unwrap(), [7], "taken in as a local entry");
}
