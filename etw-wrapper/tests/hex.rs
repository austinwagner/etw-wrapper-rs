//! Verifies that hexadecimal event descriptor attributes are accepted.

use etw_wrapper::gen_etw_wrapper;

gen_etw_wrapper!("manifests/hex.man");

#[test]
fn registers_and_emits_event_with_hex_descriptor_values() {
    let logger = ProviderHexLogger::register().expect("provider registration failed");

    logger
        .hex_event()
        .expect("emitting event with hexadecimal descriptor values failed");
}
