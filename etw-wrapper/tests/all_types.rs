//! Tests macro support for all input types.

use etw_wrapper::field::SidBuf;
use etw_wrapper::{FILETIME, GUID, SYSTEMTIME, gen_etw_wrapper};

gen_etw_wrapper!("manifests/all-types.man");

#[test]
fn registers_and_emits_all_types() {
    let logger = ProviderAllTypesLogger::register().expect("provider registration failed");

    let guid = GUID::from_u128(0);
    let filetime = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let systemtime = SYSTEMTIME::default();
    let ansi: &[u8] = b"ansi\0";
    let binary: &[u8; 16] = b"0123456789abcdef";
    let sid = SidBuf::new([0, 0, 0, 0, 0, 1], &[0]).unwrap();

    let res = logger.all_types(
        1i8, 2u8, 3i16, 4u16, 5i32, 6u32, 7u32, 8i64, 9u64, 10u64, 11.0f32, 12.0f64, 13usize, guid,
        true, "unicode", ansi, binary, filetime, systemtime, &sid,
    );

    res.expect("emitting all-types event failed");
}
