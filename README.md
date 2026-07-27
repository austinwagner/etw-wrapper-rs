# etw-wrapper

Strongly-typed [Event Tracing for Windows (ETW)](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/event-tracing-for-windows) logger generator for Rust.

Point the `gen_etw_wrapper!` macro at an ETW manifest file to emit a struct with a method for each event.

```rust
use etw_wrapper::{gen_etw_wrapper, FILETIME};

gen_etw_wrapper!("manifests/widgetservice.man", PROVIDER_WIDGETSERVICE -> WidgetLogger);

fn main() -> etw_wrapper::Result<()> {
    let logger = WidgetLogger::register()?;

    // Signatures are derived from the manifest templates
    logger.service_started("1.0.0", 8, FILETIME::default())?;
    logger.request_failed(0x12ABCDEF, 500, 42, "failed to succeed")?;

    Ok(()) // provider is unregistered automatically when `logger` is dropped
}
```

> [!NOTE]
> This crate is Windows-only. Use cfg gates if you need to use this alongside code for other platforms.

The Windows APIs are linked directly, so there is no dependency on the `windows` crate and no
version to keep in sync with the rest of your dependency tree. Types such as `GUID`, `FILETIME`,
and `EVENT_DESCRIPTOR` are defined by this crate and are layout-compatible with their Win32
counterparts.

## Getting started

Add the crate to your `Cargo.toml`:

```toml
[dependencies]
etw-wrapper = "0.1.0"
```

The `gen_etw_wrapper!` macro is available by default through the `macro` feature. If you only want the runtime pieces, disable default features:

```toml
[dependencies]
etw-wrapper = { version = "0.1.0", default-features = false }
```

See the [Manual API](#manual-api) section for how to use the helpers directly.

## Usage

### Generating a provider

Invoke the macro at module scope with the path to your manifest. Paths are resolved relative to the crate's `CARGO_MANIFEST_DIR` (i.e. the directory containing your `Cargo.toml`).

This will generate a struct using the default naming convention of `[PascalCaseProvideSymbol]Logger`. For example the symbol `PROVIDER_WIDGETSERVICE` becomes `ProviderWidgetserviceLogger`.

```rust
gen_etw_wrapper!("manifests/widgetservice.man");
```

To pick your own struct name, map the provider's manifest `symbol` to it with `->`:

```rust
gen_etw_wrapper!("manifests/widgetservice.man", PROVIDER_WIDGETSERVICE -> WidgetLogger);
```

If the provider symbol is not a valid Rust identifier, quote it:

```rust
gen_etw_wrapper!("manifests/widgetservice.man", "Contoso.WidgetService" -> WidgetLogger);
```

### Configuring event errors

Generated event methods return `etw_wrapper::Result<()>` by default. Applications that treat logging as best-effort can configure event methods to return `()` and ignore errors encountered while preparing or writing an event:

```rust
gen_etw_wrapper!(
    "manifests/widgetservice.man",
    event_methods_return_unit = true,
    PROVIDER_WIDGETSERVICE -> WidgetLogger,
);
```

This setting applies only to generated event methods. `WidgetLogger::register()` continues to return `etw_wrapper::Result<WidgetLogger>`.

Event methods can also panic on invalid input, or on every error:

```rust
gen_etw_wrapper!(
    "manifests/widgetservice.man",
    event_methods_return_unit = true,
    panic_on_input = cfg(debug_assertions),
    PROVIDER_WIDGETSERVICE -> WidgetLogger,
);
```

Input and Windows write failures are configured independently:

| Option             | Errors covered                                                          |
|--------------------|-------------------------------------------------------------------------|
| `panic_on_input`   | Errors detected while validating or preparing caller-provided fields.   |
| `panic_on_write`   | Errors returned by the Windows event writer.                            |

Both options accept `true`, `false` (the default), or a Rust `cfg(...)` predicate:

```rust
gen_etw_wrapper!(
    "manifests/widgetservice.man",
    panic_on_input = true,
    panic_on_write = cfg(feature = "strict-etw"),
);
```

Predicates are evaluated in the crate invoking the macro. When one is false, errors in that category are returned unless `event_methods_return_unit = true`. The generated method's return type depends only on `event_methods_return_unit`, which accepts only `true` or `false`, so it does not change between build configurations. Feature names used in a predicate must be declared by the crate invoking the macro.

### Emitting events

Given this manifest template and event:

```xml
<template tid="T_ServiceStarted">
  <data name="Version"     inType="win:UnicodeString"/>
  <data name="WorkerCount" inType="win:UInt32"/>
  <data name="StartTime"   inType="win:FILETIME"/>
</template>

<event value="1" symbol="SERVICE_STARTED" channel="OperationalChannel"
       level="win:Informational" template="T_ServiceStarted"/>
```

the macro generates:

```rust
impl WidgetLogger {
    pub fn register() -> etw_wrapper::Result<Self> { /* ... */ }

    pub fn service_started(&self, version: &str, worker_count: u32, start_time: FILETIME)
        -> etw_wrapper::Result<()> { /* ... */ }
}
```

> [!IMPORTANT]
> The macro emits events but does not build or register the provider's message and metadata resource tables. Consumers will need the manifest registered seperately. This can be handled via a combination of `mc.exe` and `wevtutil.exe`.
> 
> For a simple way of compiling and embedding the resources, use a build-time helper such as [`embed-resource`](https://crates.io/crates/embed-resource).

## Type mapping

The macro maps manifest `inType` values to Rust parameter types as follows:

| Manifest `inType`                                           | Rust type      | Notes                                                                            |
|-------------------------------------------------------------|----------------|----------------------------------------------------------------------------------|
| `win:Int8`                                                  | `i8`           |                                                                                  |
| `win:UInt8`                                                 | `u8`           |                                                                                  |
| `win:Int16`                                                 | `i16`          |                                                                                  |
| `win:UInt16`                                                | `u16`          |                                                                                  |
| `win:Int32`                                                 | `i32`          |                                                                                  |
| `win:UInt32`, `win:HexInt32`                                | `u32`          |                                                                                  |
| `win:Int64`                                                 | `i64`          |                                                                                  |
| `win:UInt64`, `win:HexInt64`                                | `u64`          |                                                                                  |
| `win:Float`                                                 | `f32`          |                                                                                  |
| `win:Double`                                                | `f64`          |                                                                                  |
| `win:Boolean`                                               | `bool`         | Encoded as a 32-bit Windows `BOOL`                                               |
| `win:Pointer`                                               | `usize`        |                                                                                  |
| `win:GUID`                                                  | `GUID`         |                                                                                  |
| `win:FILETIME`                                              | `FILETIME`     |                                                                                  |
| `win:SYSTEMTIME`                                            | `SYSTEMTIME`   |                                                                                  |
| `win:UnicodeString`                                         | `&str`         | Interior NULs become spaces, `length="N"` or `length="Field"` produces exactly that many units including NUL |
| `win:AnsiString`                                            | `&[u8]`        | Caller supplies provider-code-page bytes and the final NUL                       |
| `win:AnsiString` with `outType="win:Utf8/win:Json/win:Xml"` | `&str`         | Encoded as UTF-8, interior NULs become spaces                                    |
| `win:Binary`                                                | `&[u8]`        | `length="N"` becomes `&[u8; N]`, `length="Field"` derives that field (see below) |
| `win:SID`                                                   | `&field::Sid`  | Build from a raw SID pointer via `unsafe Sid::from_psid`, or borrow an owned `SidBuf` |

### Arrays

The macro supports the manifest `count` attribute for every input type in the table above.
A constant count is enforced at the type level:

```xml
<data name="Values" inType="win:UInt32" count="3"/>
```

```rust
fn event(&self, values: &[u32; 3]) -> Result<()>
```

A field-reference count is derived from the array and is not exposed as a separate parameter:

```xml
<data name="ValueCount" inType="win:UInt16"/>
<data name="Values"     inType="win:UInt32" count="ValueCount"/>
```

```rust
fn event(&self, values: &[u32]) -> Result<()>
```

Scalar arrays use `&[T; N]` or `&[T]`. String arrays use arrays or slices of `&str`; provider-code-page ANSI strings use `&[u8]`; SID arrays use `&field::Sid`. The generated method packs variable-length elements into the contiguous layout expected by ETW. Fixed-length binary arrays use `&[[u8; LENGTH]; COUNT]` for two-dimensional fixed sizes. When both binary `length` and`count` are field references, the method accepts a slice of byte slices, derives both fields, and returns `ERROR_INVALID_DATA` if the elements do not all have the same length.

### Variable-Length Fields

#### Binary

When a `win:Binary` field uses `length="OtherField"`, the raw event structure expects to receive the length in that field. The generated method does not expose this field. It is derived internally from the slice's length.

As an example:

```xml
<data name="BlobSize" inType="win:UInt32"/>
<data name="Blob"     inType="win:Binary" length="BlobSize"/>
```

generates the method 

```rust
fn blob_written(&self, blob: &[u8]) -> Result<()>
```

#### Strings

For `win:UnicodeString length="N"`, the generated method emits exactly N UTF-16 code units as required by ETW: at most N-1 content units, space padding when needed, and a terminating NUL.

A default `win:AnsiString` uses the provider's ANSI code page, so its generated parameter remains raw bytes. A fixed-length default ANSI field becomes `&[u8; N]`; the array must already contain the required padding and final NUL. If the manifest explicitly selects `outType="win:Utf8"`, `win:Json`, or `win:Xml`, the generated method accepts `&str`, performs UTF-8 encoding, and handles fixed-length padding and truncation.

A string `length="OtherField"` names the field carrying the width, which decoders read instead of scanning for a NUL. Unlike a binary length, that field stays a parameter, because it sets the width rather than describing a value the macro already has:

```xml
<data name="NameLength" inType="win:UInt16"/>
<data name="Name"       inType="win:UnicodeString" length="NameLength"/>
```

```rust
fn named(&self, name_length: u16, name: &str) -> Result<()>
```

Each call encodes `name` to exactly `name_length` code units, padding or truncating as it does for a constant length, and returns `Error::EmptyFixedLengthString` if the width leaves no room for the terminator. A default `win:AnsiString` arrives already encoded, so its buffer must match the declared width itself; a mismatch returns `Error::LengthMismatch`.

## Manual API

If you prefer not to use the macro, you can use the runtime directly. The `EtwLogger` automatically handles the internal callbacks and ensures that deregistration happens when the logger is dropped. It can be moved and stored freely.

The `field` module provides converters for passing fields to the `write` function safely: `scalar` for any `Copy` value; `str16`/`to_u16cstring` and `to_u16cstring_fixed_len` for UTF-16 strings; `str8`/`to_cstring` and `to_cstring_fixed_len` for ANSI strings; `bytes` for binary blobs; and `sid` for a `Sid`.

The converters that can reject their input return `Result`, reporting the same errors the generated event methods do: a buffer that is not NUL-terminated gives `Error::MissingNulTerminator`, and a fixed length with no room for the terminator gives `Error::EmptyFixedLengthString`.

Before logging you can call `enabled()` to check that your event write won't be a no-op.

```rust
use etw_wrapper::{EVENT_DESCRIPTOR, EtwLogger, GUID, field::*};

let ctx = EtwLogger::register(&GUID::from_u128(0x8B3A1F42_6C7D_4E9A_9F21_3D5E0A7C1B84))?;

let descriptor = EVENT_DESCRIPTOR { Id: 1, Level: 4, ..Default::default() };

if ctx.enabled(descriptor.Level, descriptor.Keyword) {
    let version = to_u16cstring("1.0.0");
    let workers = 8u32;
    ctx.write(&descriptor, &[str16(&version)?, scalar(&workers)])?;
}
```

## License

Licensed under the [ISC License](LICENSE).
