# Guarantees

What an implementation of bus-contract guarantees, what it does **not**
guarantee, and which conformance check proves each property.

The contract is split into five parts. **core** is the `Bus` trait,
which every implementation offers. The four others are optional
capabilities, which a bus may or may not offer: **defer**, **timings**,
**journal** and **crossing** (`Relay`, on top of the journal).

Each property has a **stable id**, `<contract>.<area>.<property>`. Ids
are never reused for another meaning: a property that changes meaning
gets a new id, and a property that is dropped keeps its id retired. The
"Checked by" column names the check of the conformance suite of
[`bus-kernel`](https://github.com/quazardous/bus-kernel) (0.2.1) that
proves the property on any implementation, or says "not covered yet".
"Partly" means the check covers part of the property, and the rest is
named.
Each check of that suite names the ids it proves (`Check::proves`),
and the suite runs one contract's checks on their own
(`conformance::run_contract`) as well as all of them.

This page restates the promises written on the traits (`Bus`, `Defer`,
`Timings`, `Journal`, `Relay`) and adds the non-guarantees they imply.
Where the two disagree, the trait docs win and this page is a bug.

## Read this first

These hold across every contract. Each one is also a row of a table
below.

- **A refusal never rolls back.** A request stopped by a veto, or by a
  failure on a fail-closed point, does not undo what the listeners
  before it did: their side effects stay, and the returned value holds
  their edits (`core.request.no-rollback`).
- **Requests are fail-open unless declared otherwise.** A failing
  listener (an error, a panic, a missed deadline) has its edit dropped
  and the others run. A point is fail-closed only when its declaration
  says so (`fail_closed`), or when it is a relayable request, which is
  always fail-closed (`core.request.fail-closed`). An application may
  therefore keep a request fail-open on purpose. For example, Helios
  keeps its quit request fail-open: a listener that breaks or is late
  is logged and reported, and the quit goes on, while a listener that
  refuses still vetoes it. What the contract asks is that such a choice
  be declared (not setting `fail_closed`) and documented by the
  application; a request that must not go on when a check breaks is
  declared `fail_closed`.
- **`PointKind` is not purely declarative.** `Request` changes the
  semantics: only a request can be refused or be fail-closed, and a
  refusal on another kind counts as a failure. `Event` gates the
  journal: only an event point may be journaled. Alter and collect
  never cross a process. `Alter` and `Collect` are otherwise labels:
  the caller picks the emit (`alter` or `collect`)
  (`core.kind.not-declarative`).
- **Crossing is not a distributed transaction.** Both sides agree on
  the outcome of an ask, not on its effects. An effect the far side
  produced before a deadline ran out is not undone, so listeners of
  relayable requests should be idempotent or undo their own work
  (`crossing.ask.no-transaction`).
- **Authorities protect a trusted process from mistakes.** They are not
  a security boundary against an attacker: they see what the link and
  the codec give them, inside a process that already trusts its bridge
  (`crossing.authority.not-security`).
- **No persistence, no exactly-once across processes.** The journal
  keeps a window in memory. Nothing is delivered after a crash, numbers
  start again with the process, and an ask is answered once per run of
  the asker, not once ever (`journal.window.memory-only`,
  `crossing.ask.once-per-origin`).

## core

The `Bus` trait: points, their listeners in a declared order, alter
and collect emits, requests, deadlines, local listeners, removal and
introspection.

| Property | Guaranteed | Not guaranteed | Checked by |
|---|---|---|---|
| `core.declare.once-typed` | A point is declared once: a second declaration is refused (`Refused::AlreadyDeclared`). An id is registered with one `Point` type: a listener or emit with another type is refused (`Refused::TypeMismatch`) and runs no listener. | The naming rule for ids (`<crate>.<subject>.<moment>`) is documented, not enforced: ids are plain strings. | `declare.once_and_typed` |
| `core.declare.table-checked` | `declare_all` refuses, with the point left undeclared, a point needing a capability the bus does not offer (`Refused::Unsupported`), a journaled point that is not an event (`NotEvent`), a relayable point that cannot cross (`NotCrossable`) and a codec of another point (`TypeMismatch`). | These checks are made by `declare_all`. A library that calls `declare_point` directly relies on what its implementation checks. | `declare.missing_capability_refused`; partly `crossing.declared_requests_fail_closed_alter_never` |
| `core.order.declared` | Listeners run ordered by `after` / `before` first, then by priority (higher first), then by registration. | No order between two emits running at the same time on different threads. No order across points. | `order.constraints_priority_registration` |
| `core.order.cycle-refused` | A listener whose `after` / `before` would close a cycle is refused (`Refused::Cycle`, with the path), with nothing changed. | | `order.cycle_refused_unchanged` |
| `core.order.missing-target` | A constraint naming a listener that is not registered is accepted, reported by each emit (`FailureKind::MissingTarget`, not sent to the failure sinks) and by introspection, and ignored. The listener still runs and its edit counts. | | `order.missing_target_reported_ignored` |
| `core.order.last` | A listener placed last (`ListenOpts::last`, the target `LAST`) runs after every other listener of its point. One per point: a second one is refused as a cycle. | | `order.last_runs_after_all_alone` |
| `core.alter.chain` | An alter emit passes the value through the listeners in order, each editing what the previous ones left. A listener that fails leaves the value as it found it (the value is restored from a `clone`). The returned value is of the point's type. | The value is not restored for a listener that succeeded, even if a later one fails. | `emit.alter_keeps_successful_edits` |
| `core.collect.fresh-parts` | A collect emit gives each listener a fresh default value and returns the values of the listeners that succeeded, in listener order, each with its listener's id. | | `emit.collect_gathers_fresh_parts` |
| `core.failure.isolated` | A listener's edit counts only when it returns `Ok` within its deadline without panicking. Otherwise its edit is dropped, the other listeners run, and the failure (error, panic or missed deadline) is returned in listener order. | The side effects of a failed listener are not undone. Catching a panic needs the panic strategy `unwind`; under `panic = "abort"` a panicking listener ends the process. | `emit.alter_keeps_successful_edits`, `emit.failure_sinks_hear_dropped_only` |
| `core.failure.sinks` | Each failure that dropped an edit (every kind but a missing target) is sent to the failure sinks as it happens. A sink that panics is ignored. | No order is promised between failures of emits running on different threads. | `emit.failure_sinks_hear_dropped_only` (a panicking sink included) |
| `core.deadline.inline-late` | An inline listener (the default) that returns past its deadline counts as failed (`FailureKind::Late` with its time) and its edit is dropped. | An inline listener cannot be stopped: the emit waits for it to return, so its deadline does not bound the emit. | `deadline.inline_late_fails` |
| `core.deadline.threaded-abandoned` | A threaded listener (`ListenOpts::threaded`) runs on a thread of its own, and the emit stops waiting at its deadline: it counts as late (`took: None`) and its edit is dropped. | The abandoned thread is not killed: it runs to its end and its side effects stay. | `deadline.threaded_abandoned` |
| `core.reentrant.no-lock` | Listeners run with no lock of the bus held: a listener may emit and may listen. | Whether a listener added or removed on a point during an emit of that same point runs in that emit is not specified. | `emit.reentrant_and_cross_thread`, `emit.listen_on_the_emitting_point` (no block, and the next emit sees the change) |
| `core.thread.shared` | A bus is `Send + Sync` and may be emitted on from several threads at once. Non-local listeners are `Send + Sync` and run on the emitting thread, or on a thread of their own when threaded. | Two emits on different threads are not serialized against each other. | `emit.reentrant_and_cross_thread` |
| `core.request.veto-stops` | On a `PointKind::Request` point, a listener that returns `HookError::Refuse` stops the request: its own edit is dropped, no later listener runs (on this thread or another: nothing is queued), and the result names the refusal (`refused`: point, listener, owner, reason). A refusal is not a failure and reaches no failure sink. | See `core.request.no-rollback`. | `request.refusal_stops_not_a_failure`, `local.refused_request_queues_nothing` |
| `core.request.refuse-elsewhere-fails` | On an alter, collect or event point, `HookError::Refuse` counts as a failure (`FailureKind::Error` with the reason): the others run. | | `request.refusal_off_a_request_fails` |
| `core.request.fail-closed` | On a request point declared `fail_closed`, a listener that fails stops the request as a veto does: the failure is reported as usual, and the result names a refusal with `failed: true`. A threaded listener that misses its deadline stops it at once. A relayable request is always fail-closed. | Requests are **fail-open by default**: a request is fail-closed only when declared so. `fail_closed` is ignored on the other kinds. | `request.fail_closed_refuses`; the relayable case: `crossing.declared_requests_fail_closed_alter_never` |
| `core.request.no-rollback` | A refusal, by a veto or by a failure on a fail-closed point, never rolls back the effects of the listeners that ran before it. The returned value is the value as those listeners left it. | No undo, no compensation: a listener that must not act before a later check passes is placed after that check. | `request.refusal_keeps_earlier_effects` |
| `core.kind.not-declarative` | `PointKind` changes semantics: `Request` enables refusals and `fail_closed`; `Event` is the only kind that may be journaled; a relayable event is journaled, a relayable request is fail-closed; alter and collect never cross. | `Alter` and `Collect` do not choose the emit: the caller does. | Partly `request.refusal_off_a_request_fails`, `journal.refuses_off_journaled`, `crossing.declared_requests_fail_closed_alter_never` |
| `core.local.bound` | A local listener (`listen_local`) runs only on the thread that registered it, in its place in the point's order (`after` / `before` / `LAST`, priority, registration), and shows in introspection (`local`). An emit on its thread runs it inline with the others. A local listener asking for a thread of its own is refused (`Refused::Threaded`). | | `local.ordered_and_queued` |
| `core.local.queued` | An emit on another thread runs every other listener in order and queues the local ones for their thread: the result names them (`deferred`) and the wakers are called with the thread's id. That thread runs them on `run_pending`, in their order, emits in the order they were queued. A deferred alter's listeners edit a copy of the value the emit returned; a deferred collect's listeners each fill a fresh value. | The emitter does not see what deferred listeners do: `run_pending` reports it. When the host calls `run_pending` is the host's business. A waker that panics is ignored. | `local.ordered_and_queued`, `local.deferred_collect_fresh`, `local.queued_emits_run_in_order`, `local.panicking_waker_ignored` |
| `core.local.error-boundary` | A deferred local listener that fails or panics is isolated as any other: its failure is reported by `run_pending`, and the others run. | | `local.error_boundary` |
| `core.remove.no-run-after` | A listener removed by `unlisten` or `forget`, from any thread, never runs again, queued or not. `forget` removes every listener of an owner on every point. | | `remove.unlisten_and_forget`, `local.forget_from_any_thread` |
| `core.introspect.points` | `points()` lists every point in id order with its listeners in run order; `listeners()` gives one point's. Each listener shows its owner, constraints, priority, effective deadline, threaded and local flags, missing targets, runs, failures and last failure. | Counters are not timings: durations belong to the timings capability. | `introspect.points_listeners_counters` |

## defer

The `Defer` capability (`Bus::defer`): a request asked now, whose
listeners run at the next drain on the same thread.

| Property | Guaranteed | Not guaranteed | Checked by |
|---|---|---|---|
| `defer.ask.returns-at-once` | An ask is queued for the calling thread and returns at once with its number, increasing in ask order across the bus. The wakers are called with the thread's id. | The asker gets only the number, never the outcome. | `defer.drain_in_ask_order`, `defer.ask_wakes_its_thread` |
| `defer.drain.ask-order` | `drain` runs the asks queued for the calling thread in ask order, each as an alter emit with the listeners registered when it runs. Every core rule holds, refusals and fail-closed points included. | A listener removed between the ask and the drain does not run, and one added since does: the ask does not freeze its listeners. | `defer.drain_in_ask_order`, `defer.listeners_read_at_the_drain` |
| `defer.drain.outcome-reported` | Each ask's outcome is reported by the drain (`PendingRun` with `ask` set): the listeners that ran, their failures, a refusal. | | `defer.drain_in_ask_order` |
| `defer.drain.later-asks-wait` | An ask made during a drain waits for the next drain. An immediate emit made after an ask, before the drain, runs before it. | | `defer.drain_in_ask_order` |
| `defer.thread.per-thread` | Asks are queued per thread: a drain runs only the calling thread's asks. `waiting()` counts them. | No cancel of an ask. Asks never drained never run: a host checks `waiting()` is zero before a clean exit. | `defer.asks_per_thread` |

## timings

The `Timings` capability (`Bus::timings`): how long listeners take, per
point and per listener, and passes over a point's budget.

| Property | Guaranteed | Not guaranteed | Checked by |
|---|---|---|---|
| `timings.runs.timed` | Every listener run is timed, whatever its outcome, and counted in the listener's and the pass's stats until `reset`. A threaded listener that misses its deadline counts the time the emit waited. | | `timings.timed_until_reset`, `timings.threaded_late_counts_the_wait` |
| `timings.stats.exact-counts` | Counts, totals and maxima are exact. | Quantiles (`p50`, `p95`) may be approximate. | Partly `timings.timed_until_reset` |
| `timings.budget.reported-not-failed` | A pass of a point with a budget that takes longer is reported to the budget sinks, on the thread that ran it, after the pass, with each listener's time. The pass's results are kept. A sink that panics is ignored. | A budget is not a deadline: a pass over it is never failed. On a bus without `Timings` a budget is only shown. | `timings.over_budget_reported` (a panicking sink included) |
| `timings.cost.low` | Timing is meant to cost little next to a listener: clock reads and counters, no allocation per run. | This is a performance goal, measured by benchmarks, not a conformance property. | Not covered (benchmarks, not conformance) |

## journal

The `Journal` capability (`Bus::journal`): events of journaled points
kept with a number, read again from one, delivered after the publisher
has moved on.

| Property | Guaranteed | Not guaranteed | Checked by |
|---|---|---|---|
| `journal.seq.ordered` | Numbers start at 1 and increase by one with each entry, never reused while the process runs. The journal is ordered by them. | Numbers start again with the process. An implementation may also keep entries that are not events of a point (its own facts, events relayed in from a kind not declared here). | `journal.numbers_grow_by_one`, `journal.delivered_in_order_off_publisher` |
| `journal.publish.never-waits` | Publishing records the entry and returns its number: it never runs a listener and never waits on one. | | `journal.delivered_in_order_off_publisher` |
| `journal.publish.journaled-only` | Publishing on a point not declared journaled on this bus is refused (`Refused::NotJournaled`), as is one declared with another type (`TypeMismatch`), with nothing recorded. | | `journal.refuses_off_journaled` |
| `journal.delivery.once-in-order` | Each entry of a journaled point is delivered to the point's listeners once, entries in number order, each as a collect emit (every core rule holds), on a thread of the implementation's, not the publisher's. | A slow listener delays the following entries, never the publisher; deadlines bound it. Exactly-once holds inside one process only. | `journal.delivered_in_order_off_publisher`; local listeners queued from the pump: not covered yet |
| `journal.read.since` | `since(seq, limit)` returns the entries after `seq`, oldest first, and reports a gap when some were dropped from the window before the read. | | `journal.delivered_in_order_off_publisher`, `journal.since_reports_the_gap` (the gap on a window of up to 16384 entries) |
| `journal.read.wait-flush` | `wait` returns the first entry after a number matching one of the patterns, at once if it is already there, else waiting up to a timeout. `flush` waits until every entry recorded before the call is delivered, up to a timeout. A cause given to `publish_caused` is kept on the entry. | | `journal.wait_and_wait_for`, `journal.cause_kept`; `flush`: used by `journal.delivered_in_order_off_publisher` |
| `journal.window.memory-only` | The journal keeps a window of entries. | No persistence beyond the window, no delivery after a crash or a restart. A reader that missed entries rebuilds what it follows from the current state. | Not covered (a non-guarantee) |

## crossing

The `Relay` capability (`Bus::relay`), on top of the journal: what may
cross a process boundary and what it means when it does. The transport
(framing, sockets, reconnection, discovery) is not part of the
contract.

| Property | Guaranteed | Not guaranteed | Checked by |
|---|---|---|---|
| `crossing.relay.implies-journal` | A bus that offers `Relay` offers `Journal`; `offers(bus, Capability::Relay)` checks both. | | `crossing.relay_implies_journal` |
| `crossing.declare.what-crosses` | Only an event point (with `Codec::of`) or a request point (with `Codec::request`, which writes the value too) crosses. A relayable event is journaled; a relayable request is fail-closed and not journaled. Alter and collect points never cross (`Refused::NotCrossable`). | | `crossing.declared_requests_fail_closed_alter_never` |
| `crossing.version.same-only` | Two processes cross a point only at the same version: an entry or an ask of another version is refused (`IngestError::Version`, or a refused answer). | Compatibility inside a version is the codec's job: a decoder should accept fields it does not know. | `crossing.same_version_only` |
| `crossing.resume.cursor-order` | `resume` returns the relayable entries after the cursor, in number order, each written by its codec with its version, at most `limit`, and the cursor to resume from. | | `relay.resume_and_ingest` |
| `crossing.resume.lost-reported` | A cursor of another run gets the run's entries from its start with `Lost::Restarted`; a cursor whose next entries left the window gets the kept ones with `Lost::Gap`. `Cursor::START` loses nothing. | Lost entries are not sent again: the reader resyncs from the current state. | `crossing.lost_entries_reported` (the gap on a window of up to 16384 entries) |
| `crossing.ingest.delivered` | `ingest` records an entry relayed in with its origin. A point declared here is decoded with its codec and delivered like an entry published here; a kind not declared here is kept with its text and delivered to no one. A point declared here but not relayable, or at another version, or that does not decode, is refused with nothing recorded. | | `relay.resume_and_ingest`, `crossing.same_version_only` |
| `crossing.ingest.no-echo` | Entries taken in are not served again by `resume`: two processes relaying to each other do not echo. | | `crossing.ingested_not_echoed` |
| `crossing.origin.from-link` | Whatever crosses in carries an `Origin` (process, and agent for an agent's session), kept on every entry taken in and checked on every ask. | The contract cannot check where the origin came from: setting it from the link, never from what the sender wrote, is the bridge's duty. | `crossing.origin_kept_on_entries` |
| `crossing.authority.first` | Whatever crosses in (an entry to ingest, an ask to answer) is shown to every authority, in the order they were added, before anything is decoded, recorded or run. One refusal refuses it; an authority that panics refuses. With no authority, everything relayable is taken. | | `crossing.authority_checks_asks_first`, `crossing.authority_checks_events_panic_refuses` |
| `crossing.authority.not-security` | Authorities protect a trusted process against mistakes: a library or an agent asking for something it should not. | They are not a security boundary against an attacker. They see only what the link and the codec report, and they run inside a process that trusts its bridge and its peers' identities. Authentication, encryption and isolation belong to the transport and the host. | Not covered (a non-guarantee) |
| `crossing.answer.same-meaning` | `answer` runs a relayable request's listeners here as an alter emit with the ask's input and value read by the codec, and answers with the value they left or the refusal. Every core rule holds. A listener cannot tell whether the far side is in its process or another. | | `crossing.answered_far_means_the_same` |
| `crossing.answer.veto-reason` | A far listener's refusal comes back as `Answer::Refused` naming the listener and its reason, and refuses the request on the asking side with that reason. A refusal by the far bus itself names no listener. | | `crossing.veto_carries_its_reason` |
| `crossing.answer.refuses-unknown` | An ask on a point not declared here, not relayable, at another version, or whose input or value cannot be read, is refused and runs no listener. | | `crossing.not_a_relayable_request_refused`, `crossing.same_version_only` |
| `crossing.deadline.fail-closed` | On the asking side (`far_listener`), no answer in time, an answer after the deadline, an answer to another ask, or an answer it cannot read refuses the request: no answer means no. | | `crossing.no_answer_in_time_refuses` |
| `crossing.deadline.far-side-keeps` | The far side counts the deadline from when it got the ask (clocks are not compared), runs nothing when it has passed, and answers a refusal (failed) when its listeners end past it, whatever they did: both sides hold the same outcome. | See `crossing.ask.no-transaction`. | `crossing.far_side_keeps_the_deadline` |
| `crossing.ask.once-per-origin` | An ask is named by the asker's run and number. The far side runs its listeners once per origin process: a repeat gets the first answer again while it is kept (at least as long as the ask's deadline), else a refusal. | Not exactly-once across processes or restarts: an asker that restarts starts a new run, and its asks are new asks. | `crossing.answered_once_per_origin` |
| `crossing.ask.no-transaction` | Both sides agree on the outcome of an ask. | **Not a distributed transaction.** Effects the far side's listeners produced before the deadline ran out are not undone when the answer is a refusal. Listeners of relayable requests should be idempotent, or undo their own work. | Not covered (a non-guarantee) |
| `crossing.ask.blocks-asker` | An ask through `far_listener` is synchronous for the asker, bounded by its deadline. | The emitting thread blocks up to the deadline, so a crossing request is not emitted from a thread that must stay responsive (an interface thread). Registering the far listener `threaded` lets the emit stop waiting at the deadline even when the transport hangs. | Not covered (documented behaviour) |

## Credits

The style of this page, and its "what we do not guarantee" framing,
come from [eventbus-contract](https://github.com/mapseekai/eventbus-contract)
and [qubit-event-bus](https://github.com/qubit-ltd/rs-event-bus), which
both state plainly what their implementations promise and what they do
not.
