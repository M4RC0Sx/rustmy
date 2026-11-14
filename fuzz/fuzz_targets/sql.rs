#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| rustmy_proto::fuzzing::sql(data));
