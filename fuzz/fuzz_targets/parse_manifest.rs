#![no_main]
use libfuzzer_sys::fuzz_target;
use std::io::Write;

fuzz_target!(|data: &[u8]| {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Skillfile");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(data).unwrap();
    drop(f);
    let _ = skillfile_core::parser::parse_manifest(&path);
});
