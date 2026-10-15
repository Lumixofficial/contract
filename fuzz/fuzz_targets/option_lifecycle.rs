#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| lumix_test_suites::scenarios::option_lifecycle(data));
