# bus-kernel

[![crates.io](https://img.shields.io/crates/v/bus-kernel.svg)](https://crates.io/crates/bus-kernel)
[![docs.rs](https://docs.rs/bus-kernel/badge.svg)](https://docs.rs/bus-kernel)

A small hook kernel: named **hook points** where several parts of a
program contribute, in an order they declare, to one result, and where
one failing contribution never breaks the others.

- **Typed points.** A point is a type: its id, the input every listener
  reads and the value a listener edits, checked at compile time.
- **Listeners with ids,** ordered by `after`/`before` first, then by
  priority (higher first), then by registration order: the same
  listeners always run in the same order. A cycle is refused when the
  listener registers, naming the listeners in it; a constraint naming a
  listener that is not there is reported and ignored.
- **Two emits on one listener model.** `alter` passes one value through
  the listeners in order, each may change it; `collect` gives each
  listener a fresh value and gathers them in a list.
- **An error boundary per listener.** A listener's edit counts only when
  it returns `Ok`, without panicking (panics are caught), within its
  deadline. Otherwise its edit is dropped, the others still run, and the
  emit returns the failure. Sinks receive each failure as it happens, so
  a host can publish a `hook.failed` fact or log it.
- **Deadlines.** One second by default, overridable per point and per
  listener (`PLUGIN_DEADLINE`, five seconds, for listeners that answer
  from another process). An inline listener runs on the emitting thread
  and counts as late once it returns; a threaded one runs on a thread of
  its own and the emit stops waiting at the deadline.
- **Local listeners.** `listen_local` registers a listener that is not
  `Send`, bound to the calling thread and ordered with the others. An
  emit from another thread queues it; the thread runs the queue with
  `run_pending()`, and `on_pending` wakers say when there is one.
- **Refusals on requests.** On a point declared `PointKind::Request`, a
  listener returning `HookError::refuse(reason)` stops the request: no
  later listener runs and the result names the refusal (`refused`). On a
  point declared `fail_closed`, a listener's error, panic or missed
  deadline refuses the request the same way instead of being dropped.
- **Deferred requests.** The kernel offers the contract's `Defer`
  capability: `ask::<P>` queues a request for the calling thread and
  returns at once; `drain()` runs the thread's asks in ask order and
  reports what each did; `waiting()` counts them. An ask made during a
  drain waits for the next one. A GUI application typically drains once
  per frame, after drawing, and before it quits.
- **Listener times and budgets.** The kernel offers the contract's
  `Timings` capability: each listener run is timed into a log histogram
  of relaxed atomics, per listener and per pass (`timings()`,
  `point_timings()`: count, total, max, p50, p95; `reset_timings()`;
  `set_timing(false)` to stop counting). A pass over its point's budget
  (`PointSpec::budget`) goes to the `on_over_budget` sinks, its results
  kept.
- **A journal and a relay from the host.** The host keeps the entries:
  `with_journal(journal)` (the contract's `Journal`) and
  `with_relay(host)` (a `RelayHost`: read from a cursor, record what is
  taken in) hand them to the kernel, which then accepts points declared
  journaled or relayable (refused without them) and refuses a publish on
  a point not declared journaled before the host sees it. The host's
  pump delivers each entry with a collect emit on the kernel, off the
  publisher's thread; `declaration(id)` gives it a point's spec and
  codec.
- **Crossing.** The kernel does the rest of bus-contract's crossing
  itself: it holds the relay's authorities and shows them each crossing
  before anything is decoded, recorded or run; refuses an entry of a
  point not relayable here or at another version before the host sees
  it; writes each point's version on the entries served; and answers
  asks of relayable requests (`Relay::answer`) with an alter emit on
  itself, fail-closed on the ask's deadline, once per origin (a repeat
  gets the first answer, kept at least as long as the ask's deadline
  and 30 s).
- **Introspection.** `points()` lists every point and its listeners in
  the order they run, with their owners, constraints, deadlines, missing
  targets and counters (runs, failures, the last failure), for people,
  agents and inspection endpoints.
- **An implementation of [`bus-contract`](https://github.com/quazardous/bus-contract).**
  The kernel is a `bus_contract::Bus`: a library that takes an
  `Arc<dyn Bus>` from its application runs on it, or on any other
  implementation the application prefers. The contract's types
  (`Point`, `ListenOpts`, `PointSpec`…) are re-exported here.
- One dependency (`bus-contract`, itself without any), no `unsafe`, no GUI toolkit, no async runtime: the
  kernel is `Send + Sync`, listeners are `Send + Sync` closures, local
  ones excepted.

It was written for [egui-pop](https://github.com/quazardous/egui-pop)
(where it was the `egui-pop-hooks` crate, which now re-exports this one)
and works in any Rust program, with or without a GUI. Its
ideas (listener ids, dependency ordering, an error boundary) come from
[QuarKernel](https://github.com/quazardous/quarkernel).

## Collect: each listener adds its part

```rust
use bus_kernel::{Kernel, ListenOpts, Point, PointSpec};

/// The findings of a health report: each check adds its own.
struct HealthReport;
impl Point for HealthReport {
    const ID: &'static str = "health.report";
    type Input = ();
    type Value = Vec<String>;
}

let kernel = Kernel::new();
kernel
    .declare::<HealthReport>(PointSpec::new("The findings of the health report"))
    .unwrap();
kernel
    .listen::<HealthReport>("storage.free-space", ListenOpts::new().owner("storage"), |_, out| {
        out.push("disk 91% full".into());
        Ok(())
    })
    .unwrap();
kernel
    .listen::<HealthReport>("gpu.driver", ListenOpts::new().owner("gpu"), |_, _| {
        Err("driver not found".into())
    })
    .unwrap();

let report = kernel.collect::<HealthReport>(());
assert_eq!(report.parts, [("storage.free-space".to_string(), vec!["disk 91% full".to_string()])]);
assert_eq!(report.failures.len(), 1);
assert_eq!(report.failures[0].listener, "gpu.driver");
```

## Alter: the value goes through the listeners in order

```rust
use bus_kernel::{Kernel, ListenOpts, Point};

/// The entries of a menu, which plugins may add to or reorder.
struct FileMenu;
impl Point for FileMenu {
    const ID: &'static str = "menu.file";
    type Input = ();
    type Value = Vec<&'static str>;
}

let kernel = Kernel::new();
kernel
    .listen::<FileMenu>("sort", ListenOpts::new().after("export"), |_, items| {
        items.sort();
        Ok(())
    })
    .unwrap();
kernel
    .listen::<FileMenu>("export", ListenOpts::new(), |_, items| {
        items.push("Export");
        Ok(())
    })
    .unwrap();
kernel
    .listen::<FileMenu>("broken", ListenOpts::new().priority(10), |_, items| {
        items.clear();
        panic!("half-way through");
    })
    .unwrap();

let menu = kernel.alter::<FileMenu>((), vec!["Open", "Close"]);
// `broken` ran first (priority) and panicked: its edit was dropped.
assert_eq!(menu.value, ["Close", "Export", "Open"]);
assert_eq!(menu.failures.len(), 1);
```

## Failures as facts

```rust
use std::sync::{Arc, Mutex};
use bus_kernel::{FAILED_FACT, Kernel};

let kernel = Kernel::new();
let published = Arc::new(Mutex::new(Vec::new()));
let sink = published.clone();
kernel.on_failure(move |failure| {
    // A host publishes it on its event bus.
    sink.lock().unwrap().push((FAILED_FACT, failure.to_string()));
});
```

## A conformance suite for any implementation

With the `conformance` feature, `bus_kernel::conformance` holds the
checks of what bus-contract promises: declarations, listener order
(`after`, `before`, `LAST`, priority), alter and collect, the error
boundary, refusals and fail-closed requests, removal, local listeners,
re-entrance, deadlines, introspection, and the `Defer`, `Timings`,
`Journal` and `Relay` capabilities when the bus offers them, with the
`crossing` group (requests answered on another bus, vetoes, deadlines,
answered once, origins, authorities, versions) on a bus with a relay. Another
implementation runs them from its tests:

```toml
[dev-dependencies]
bus-kernel = { version = "0.2", features = ["conformance"] }
```

```rust,ignore
#[test]
fn my_bus_keeps_the_contract() {
    bus_kernel::conformance::run(|| std::sync::Arc::new(MyBus::new()));
}
```

`run` runs every check on fresh buses and panics naming the failed
ones; `conformance::checks()` lists them for a harness of its own. A
capability the bus does not offer skips its checks. This kernel runs the
suite on itself.

## Versions

`bus-kernel` follows `bus-contract`'s minor versions: 0.1 implements
bus-contract 0.3, 0.2 implements bus-contract 0.4; a new contract minor
brings a new kernel minor.

## Next to an event bus, not on it

An event bus says what happened; nobody answers. A hook emit is a call
that builds a result and returns it with every listener's failure. A
program usually has both: the owner of a point emits, then publishes a
fact when others should know the result exists.

License: MIT OR Apache-2.0.
