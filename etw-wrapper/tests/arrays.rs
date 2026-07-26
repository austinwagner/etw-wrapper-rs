//! Tests generated APIs and event writes for fixed- and variable-count arrays.

use etw_wrapper::field::{Sid, SidBuf};
use etw_wrapper::{FILETIME, GUID, SYSTEMTIME, gen_etw_wrapper};

gen_etw_wrapper!("manifests/arrays.man");

mod ignored_event_errors {
    use etw_wrapper::gen_etw_wrapper;

    gen_etw_wrapper!(
        "manifests/arrays.man",
        event_methods_return_unit = true,
        PROVIDER_ARRAYS -> BestEffortArraysLogger,
    );

    #[test]
    fn event_methods_return_unit_while_registration_remains_fallible() {
        let registration: etw_wrapper::Result<BestEffortArraysLogger> =
            BestEffortArraysLogger::register();
        let logger = registration.expect("provider registration failed");

        let emitted: () = logger.variable_scalars(&[10, 20, 30, 40]);
        assert_eq!(emitted, ());
    }
}

mod input_panics {
    use etw_wrapper::gen_etw_wrapper;

    gen_etw_wrapper!(
        "manifests/arrays.man",
        event_methods_return_unit = true,
        panic_on_input = true,
        PROVIDER_ARRAYS -> InputPanicArraysLogger,
    );

    #[test]
    #[should_panic(expected = "invalid input for ETW event `SHARED_COUNT`")]
    fn invalid_input_panics_even_when_the_provider_is_disabled() {
        let logger = InputPanicArraysLogger::register().expect("provider registration failed");

        logger.shared_count(&[10, 20], &[1]);
    }
}

mod cfg_disabled_input_panics {
    use etw_wrapper::gen_etw_wrapper;

    gen_etw_wrapper!(
        "manifests/arrays.man",
        event_methods_return_unit = true,
        panic_on_input = cfg(any()),
        panic_on_write = cfg(any()),
        PROVIDER_ARRAYS -> CfgDisabledPanicArraysLogger,
    );

    #[test]
    fn cfg_disabled_panic_uses_the_ignore_fallback() {
        let logger =
            CfgDisabledPanicArraysLogger::register().expect("provider registration failed");

        let emitted: () = logger.shared_count(&[10, 20], &[1]);
        assert_eq!(emitted, ());
    }
}

#[test]
fn emits_fixed_scalar_arrays() {
    let logger = ProviderArraysLogger::register().expect("provider registration failed");

    logger
        .fixed_scalars(
            &[-1, 1],
            &[1, 2],
            &[-2, 2],
            &[3, 4],
            &[-5, 5],
            &[6, 7],
            &[8, 9],
            &[-10, 10],
            &[11, 12],
            &[13, 14],
            &[1.5, 2.5],
            &[3.5, 4.5],
            &[0, 1],
            &[GUID::from_u128(1), GUID::from_u128(2)],
            &[true, false],
            &[FILETIME::default(), FILETIME::default()],
            &[SYSTEMTIME::default(), SYSTEMTIME::default()],
        )
        .expect("emitting fixed scalar arrays failed");
}

#[test]
fn derives_variable_scalar_count() {
    let logger = ProviderArraysLogger::register().expect("provider registration failed");

    logger
        .variable_scalars(&[10, 20, 30, 40])
        .expect("emitting variable scalar array failed");
}

#[test]
fn validates_arrays_that_share_a_count_field() {
    let logger = ProviderArraysLogger::register().expect("provider registration failed");

    logger
        .shared_count(&[10, 20, 30], &[1, 2, 3])
        .expect("emitting arrays with a shared count failed");
}

#[test]
fn packs_string_arrays() {
    let logger = ProviderArraysLogger::register().expect("provider registration failed");
    let ansi_names: [&[u8]; 2] = [b"one\0", b"two\0"];
    let fixed_ansi_names = [*b"one\0", *b"two\0"];

    logger
        .strings(
            &["one", "two"],
            &["long name", "two"],
            &ansi_names,
            &fixed_ansi_names,
            &["café", "tea"],
            &["café", "tea"],
        )
        .expect("emitting string arrays failed");
}

#[test]
fn packs_runtime_length_string_arrays() {
    let logger = ProviderArraysLogger::register().expect("provider registration failed");
    let ansi_names: [&[u8]; 2] = [b"one\0", b"two\0"];

    logger
        .variable_length_strings(4, &["one", "two"], &ansi_names, &["one", "two"])
        .expect("emitting runtime-length string arrays failed");
}

#[test]
fn packs_fixed_and_variable_binary_arrays() {
    let logger = ProviderArraysLogger::register().expect("provider registration failed");
    let fixed = [*b"abcd", *b"efgh"];
    let variable: [&[u8]; 2] = [b"abc", b"def"];

    logger
        .fixed_binary(&fixed)
        .expect("emitting fixed binary array failed");
    logger
        .variable_binary(&variable)
        .expect("emitting variable binary array failed");
}

#[test]
fn packs_sid_arrays() {
    let logger = ProviderArraysLogger::register().expect("provider registration failed");
    let sid = SidBuf::new([0, 0, 0, 0, 0, 1], &[0]).unwrap();
    let sids: [&Sid; 2] = [&sid, &sid];

    logger.sids(&sids).expect("emitting SID array failed");
}
