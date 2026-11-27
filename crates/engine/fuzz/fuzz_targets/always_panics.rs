#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    let _ = data;
    panic!("deliberate crash to prove the failure path");
});
