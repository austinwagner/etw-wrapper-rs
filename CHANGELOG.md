# Changelog

## [Unreleased]

### Changed

* Replaced the `windows` dependency with direct ETW links, crate-owned Win32 ABI types, and a
  crate-specific `Error` type.
* Updated `safe-sid` to `v1.0.0`.
* Changed manual string helpers to return validation errors instead of panicking.
* Restricted `scalar` and `slice` to the sealed `Scalar` types ETW can serialize safely.

### Removed

* Removed the raw `EVENT_DATA_DESCRIPTOR` export; use `field::EventDataDescriptor` instead.

### Fixed

* Correctly derive field-referenced lengths for scalar arrays.
* Keep the ETW callback context at a stable address and avoid unregistering a failed registration.

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
* Updated `safe-sid` to `v0.2.0`.

## [0.1.0] - 2026-07-22

### Added

* Initial release.
