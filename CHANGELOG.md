# Changelog

## [0.2.1] - 2026-07-26

### Added

* Added `event_methods_return_unit`, `panic_on_input`, and `panic_on_write` generator options.
  * Panic options accept booleans or `cfg(...)` predicates.

## [0.2.0] - 2026-07-24

### Added

* Support manifest `count` attributes for scalar, string, binary, and SID arrays.
* Support hexadecimal integer values in XML manifests for parity with `mc.exe`.

### Changed

* Improved crate documentation.
* Simplified macro generation internals.
* Marked integration tests as requiring the default `macro` feature.
* Updated `safe-sid` to `v0.2.0` 
