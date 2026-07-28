# Changelog

## [1.0.0] - 2026-07-27

### Added

* Added a crate-specific, non-exhaustive `Error` type with distinct input-validation variants,
  access to the corresponding Win32 error code, and conversion to `std::io::Error`.
* Added the sealed `field::Scalar` trait for fixed-size values that ETW can serialize safely.
* Added macro-expansion validation for duplicate provider name overrides and constant zero-length
  string fields.
* Included the ISC license text in the published packages.

### Changed

* Removed the `windows` dependency in favor of direct ETW links through `windows-link` and private
  generated bindings.
* Replaced the re-exported Windows ABI types with crate-owned `Guid`, `FileTime`, `SystemTime`, and
  `EventDescriptor` types. Their public fields now use snake_case names, and the value types
  implement standard comparison, hashing, and debugging traits. `Guid` also supports canonical
  `u128` conversion and construction from its component fields.
* Changed the crate's `Result<T>` alias to use the new `Error` type instead of
  `windows::core::Error`.
* Updated `safe-sid` to `v1.0.0`.
* Changed `field::str8`, `field::str16`, `field::to_cstring_fixed_len`, and
  `field::to_u16cstring_fixed_len` to return validation errors instead of panicking.
* Restricted `field::scalar` and `field::slice` to `Scalar` implementations instead of accepting
  every `Copy` type.
* Added `#[must_use]` to enablement checks, descriptor builders, string encoders, and `Guid`
  conversion helpers.
* Expanded and standardized the README, crate documentation, and generated provider and event
  documentation.

### Removed

* Removed the raw `EVENT_DATA_DESCRIPTOR` export; use `field::EventDataDescriptor` instead.
* Removed the raw `PSID` export; use `field::Sid` or `field::SidBuf` instead.

### Fixed

* Honor field-referenced widths for scalar string fields, including validating caller-encoded
  provider-ANSI buffers.
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

[Unreleased]: https://github.com/austinwagner/etw-wrapper-rs/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/austinwagner/etw-wrapper-rs/compare/v0.2.1...v1.0.0
[0.2.1]: https://github.com/austinwagner/etw-wrapper-rs/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/austinwagner/etw-wrapper-rs/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/austinwagner/etw-wrapper-rs/releases/tag/v0.1.0
