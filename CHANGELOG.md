# Changelog

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
