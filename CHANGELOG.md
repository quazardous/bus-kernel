# Changelog

## 0.3.1

- Conformance: `crossing.declared_requests_fail_closed_alter_never`
  looks only at the points it declares or tries to declare. It
  expected no point at all to be declared on the bus, so it failed on
  an application's bus that holds its own points. No other check
  counts the bus's points as a whole; a new test runs the whole suite
  on a bus with application points declared and listened to before
  it runs.

## 0.3.0

Implements bus-contract 0.5 and its capability contracts at 0.2
(`bus-contract-defer`, `-timings`, `-journal`, `-crossing`). Breaking,
as the contract is; bus-contract's `MIGRATION-0.5.md` covers the
changes for users.

- The capability traits and their types come from their contract
  crates, re-exported at the crate root with each crate
  (`bus_kernel::bus_contract_journal`, …) and its extension trait
  (`BusDefer`, `BusTimings`, `BusJournal`, `BusCrossing`). The core
  getters are gone: the extension traits go through `Kernel`'s
  `Bus::capability`.
- Declarations follow the contract's facets: `declare_point` runs the
  journal and crossing contracts' checks on a point's facets and
  refuses a facet whose contract the kernel does not offer
  (`Refused::Unsupported`); `PointInfo::facets` replaces `journaled` and
  `relayable`. A publish off a journaled point is refused with the
  journal contract's `Refused::Contract`.
- Validated ids: `declare_point` refuses a point id and `listen` /
  `listen_local` a listener id that break the naming rule
  (`<prefix>.<segment>…`, `check_id`) with `Refused::BadId`, and change
  nothing. `after` / `before` targets are not checked.
- Conformance: new core checks `id.declare_refuses_bad_ids`
  (`core.id.point-checked`) and `id.listen_refuses_bad_ids`
  (`core.id.listener-checked`). `capability.getter_and_lookup_agree` is
  renamed `capability.extension_and_lookup_agree`: each capability is
  reached through its extension trait and through the lookup with the
  same answer, and `offers` agrees. The suite's listener ids carry the
  `t.` prefix.
- `conformance::GUARANTEES` is a copy of bus-contract 0.5.0's
  `GUARANTEES.md`.

## 0.2.2

The capability lookup of bus-contract 0.4.4; requires bus-contract 0.4.4.

- `Kernel` implements `Bus::capability`: `Defer` and `Timings` always,
  `Journal` with a host journal, `Relay` with a journal and a relay. The
  getters (`defer()`, `timings()`, `journal()`, `relay()`) now use the
  contract's default, this lookup, so they cannot disagree with it.
- New core check `capability.getter_and_lookup_agree`
  (`core.capability.lookup-agrees`): each capability is reached through
  its getter and through the lookup with the same answer, and `offers`
  agrees. A bus that overrides the getters without implementing
  `capability` fails it.
- `conformance::GUARANTEES` is a copy of bus-contract 0.4.4's
  `GUARANTEES.md`.

## 0.2.1

Conformance grouped by contract; additive.

- `conformance::Contract` (`core`, `defer`, `timings`, `journal`,
  `crossing`) and `Check::contract`; `checks_of`, `report_contract` and
  `run_contract` run one contract's group, `run` still runs them all.
- Each check names the property ids of bus-contract's guarantees page it
  proves (`Check::proves`); a failure lists them. The page ships with
  the crate (`conformance::GUARANTEES`, a copy of bus-contract 0.4.3's
  `GUARANTEES.md`), `properties()` reads its ids, `unproved()` lists
  those no check proves, and `summary(make)` writes every outcome with
  its ids. A test refuses an id the page does not have, and a check name
  in the page that does not exist.
- 14 new checks for the rows the page marked "not covered yet":
  `emit.listen_on_the_emitting_point`,
  `request.refusal_keeps_earlier_effects`,
  `local.queued_emits_run_in_order`, `local.panicking_waker_ignored`,
  `defer.ask_wakes_its_thread`, `defer.listeners_read_at_the_drain`,
  `timings.threaded_late_counts_the_wait`,
  `journal.numbers_grow_by_one`, `journal.since_reports_the_gap`,
  `journal.wait_and_wait_for`, `journal.cause_kept`,
  `crossing.relay_implies_journal`, `crossing.lost_entries_reported`
  and `crossing.ingested_not_echoed`. The gap checks publish up to
  `WINDOW_PROBE` (16384) entries to push one out of the window; a
  journal keeping more passes them without the gap being reached. The
  kernel passed every new check: no kernel change.

## 0.2.0

Implements bus-contract 0.4: crossing.

- The kernel answers asks of relayable requests (`Relay::answer`): an
  alter emit on itself, refused without running a listener when the
  point is not a relayable request here, at another version, its input
  or value unreadable, or its deadline gone; refused (failed) when its
  listeners end past the deadline. Each ask (origin process, run,
  number) runs once: a repeat gets the first answer, waits for it while
  it runs, and is refused once its answer is dropped (kept at least as
  long as the ask's deadline and 30 s).
- Authorities (`Relay::add_authority`, `authorize`), held by the kernel
  and shown each crossing in order before anything is decoded,
  recorded or run; a panic refuses.
- Versions: an entry of a point declared here at another version, or
  of a point that is not a relayable event here, is refused before the
  host sees it; entries served carry their point's version.
  `PointInfo::version`.
- Declaring: a relayable request with a request codec is accepted;
  a relayable alter or collect point, or a request without a value
  codec, is refused with `Refused::NotCrossable`.
- Conformance: the `crossing` group, 11 checks, run on a bus with a
  relay; `relay.resume_and_ingest` takes an `Origin` and checks it is
  kept.
- Re-exports `Answer`, `Authority`, `Crossing`, `Origin`,
  `RelayedAnswer`, `RelayedAsk`, `RelayedRequest`, `ask_run`,
  `far_listener`.

Breaking:

- `bus-contract` 0.4.
- `Kernel::with_relay` takes an `Arc<dyn RelayHost>` instead of an
  `Arc<dyn Relay>`: the host keeps `cursor`, `resume` and
  `ingest(&Origin, entry)` (recording `JournalEntry::origin`); the
  kernel does answers and authorities itself. Migration: implement
  `RelayHost` instead of `Relay`, take `&Origin` in `ingest` and set
  `entry.origin = Some(origin.clone())` instead of `entry.source`.

## 0.1.0

- First release. The hook kernel formerly published as `egui-pop-hooks`
  0.2 (its code, tests and API unchanged; `egui-pop-hooks` now
  re-exports this crate): an implementation of bus-contract 0.3 with
  ordered listeners, alter and collect emits, a per-listener error
  boundary, deadlines, local listeners, the `Defer` and `Timings`
  capabilities, and the `Journal` and `Relay` capabilities from a host.
- The `conformance` feature: `bus_kernel::conformance` (`run`,
  `report`, `checks`), the checks of bus-contract's promises any `Bus`
  implementation can run from its tests. The kernel runs them on itself.
