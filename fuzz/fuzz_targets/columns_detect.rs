//! Format detection on arbitrary line samples: no panic; every detection
//! compiles and its parser can parse the sample without panicking.
#![no_main]

use libfuzzer_sys::fuzz_target;
use oxtail_columns::detect;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let lines: Vec<&str> = text.lines().take(200).collect();
    for d in detect(&lines) {
        assert!((0.0..=1.0).contains(&d.score), "score {}", d.score);
        let parser = d
            .spec
            .compile()
            .unwrap_or_else(|e| panic!("detection {:?} does not compile: {e}", d.spec));
        for l in &lines {
            let _ = parser.parse(l);
        }
    }
});
