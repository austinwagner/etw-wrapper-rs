//! Tests for `length="OtherField"` templates: a binary length is derived from the blob rather
//! than exposed as a parameter, while a string length stays a parameter and fixes the width of
//! the encoded field.

use etw_wrapper::gen_etw_wrapper;

gen_etw_wrapper!("manifests/fieldref.man");

mod input_panics {
    use etw_wrapper::gen_etw_wrapper;

    gen_etw_wrapper!(
        "manifests/fieldref.man",
        event_methods_return_unit = true,
        panic_on_input = true,
        PROVIDER_FIELDREF -> InputPanicFieldrefLogger,
    );

    #[test]
    #[should_panic(expected = "invalid input for ETW event `ANSI_NAMED`")]
    fn malformed_caller_encoded_string_uses_the_input_policy() {
        let logger = InputPanicFieldrefLogger::register().expect("provider registration failed");

        logger.ansi_named(b"invalid!");
    }

    #[test]
    #[should_panic(expected = "invalid input for ETW event `VAR_ANSI_NAMED`")]
    fn caller_encoded_string_must_match_its_referenced_length() {
        let logger = InputPanicFieldrefLogger::register().expect("provider registration failed");

        // The declared width is 8 bytes but only 4 are supplied, which would previously have
        // been written as a short NUL-terminated buffer and decoded as 8 bytes of garbage.
        logger.var_ansi_named(8, b"abc\0");
    }

    #[test]
    #[should_panic(expected = "invalid input for ETW event `VAR_NAMED`")]
    fn referenced_length_must_leave_room_for_a_terminator() {
        let logger = InputPanicFieldrefLogger::register().expect("provider registration failed");

        logger.var_named(0, "anything");
    }
}

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
fn referenced_length_unicode_is_encoded_to_the_declared_width() {
    let logger = ProviderFieldrefLogger::register().expect("provider registration failed");

    // Name declares length="NameLength", so the payload is exactly that many UTF-16 units,
    // padded or truncated to fit, rather than terminated at the string's own length.
    logger
        .var_named(6, "hi")
        .expect("emitting padded name failed");
    logger
        .var_named(4, "far too long to fit")
        .expect("emitting truncated name failed");
}

#[test]
fn referenced_length_ansi_accepts_a_matching_buffer() {
    let logger = ProviderFieldrefLogger::register().expect("provider registration failed");

    logger
        .var_ansi_named(4, b"abc\0")
        .expect("emitting ANSI name failed");
}

#[test]
fn explicit_utf8_ansi_accepts_str() {
    let logger = ProviderFieldrefLogger::register().expect("provider registration failed");

    // win:Utf8 makes the encoding explicit, so the generated API can safely accept &str.
    logger
        .utf_8_named("café is too long")
        .expect("emitting UTF-8 named event failed");
}
