//! The lenient settings loader must never panic and must always return usable
//! (sanitised) settings, whatever the TOML looks like.
#![no_main]

use libfuzzer_sys::fuzz_target;
use oxtail_config::{DataMode, Settings};

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    for mode in [DataMode::Portable, DataMode::Installed] {
        let (settings, _warning) = Settings::from_toml_str(&text, &mode);
        // Whatever came out must serialise again.
        let _ = settings.to_toml_string();
    }
});
