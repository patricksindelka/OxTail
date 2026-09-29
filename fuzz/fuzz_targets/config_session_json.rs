//! The lenient session loader must never panic and must always return a
//! session, whatever the JSON (or non-JSON) looks like.
#![no_main]

use libfuzzer_sys::fuzz_target;
use oxtail_config::{PathMapper, Session};

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let mapper = PathMapper::absolute_only();
    let session = Session::from_json_lenient(&text, &mapper);
    // A loaded session must survive a save/load round trip.
    if let Ok(json) = session.to_json(&mapper) {
        let _ = Session::from_json_lenient(&json, &mapper);
    }
});
