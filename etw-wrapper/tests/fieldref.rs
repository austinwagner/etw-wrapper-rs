//! Tests that a `win:Binary length="OtherField"` template derives the length
//! field from the blob rather than exposing it as a parameter.

use etw_wrapper::gen_etw_wrapper;

gen_etw_wrapper!("manifests/fieldref.man");

#[test]
fn derives_length_field_from_blob() {
    let logger = ProviderFieldrefLogger::register().expect("provider registration failed");

    // The template declares BlobSize as the length field for the binary Blob
    // but the generated method takes only the blob, BlobSize is not a parameter
    // If it were still exposed this call would fail to compile due to the wrong arity
    // The UInt32 length descriptor is synthesized from blob.len() and emitted before the bytes
    logger
        .blob_written(b"hello world")
        .expect("emitting blob event failed");

    // An empty blob has a derived length of 0
    logger
        .blob_written(&[])
        .expect("emitting empty blob event failed");
}

#[test]
fn constant_length_unicode_is_accepted() {
    let logger = ProviderFieldrefLogger::register().expect("provider registration failed");

    // Name uses win:UnicodeString length="8" and is exposed as &str
    // The generated method emits exactly 8 UTF-16 units, including the terminator
    logger
        .named("this name is far too long to fit in eight")
        .expect("emitting named event failed");
}

#[test]
fn constant_length_ansi_is_accepted() {
    let logger = ProviderFieldrefLogger::register().expect("provider registration failed");

    // Default win:AnsiString fields preserve caller-provided provider-code-page bytes.
    logger
        .ansi_named(b"ansi   \0")
        .expect("emitting ANSI named event failed");
}

#[test]
fn explicit_utf8_ansi_accepts_str() {
    let logger = ProviderFieldrefLogger::register().expect("provider registration failed");

    // win:Utf8 makes the encoding explicit, so the generated API can safely accept &str.
    logger
        .utf_8_named("café is too long")
        .expect("emitting UTF-8 named event failed");
}
