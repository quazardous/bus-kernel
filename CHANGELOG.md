# Changelog

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
