//! A2.01's CI check for the event payloads (TS ≡ the Rust event types):
//! `app/ui/src/gen/events.ts` is what `gmclip_app::events` gives. Fails while they differ;
//! `cargo xt gen` (this test with `GMCLIP_GEN_WRITE=1`) writes the file again.

use std::path::Path;

#[test]
fn the_ui_event_types_match_the_rust_ones() {
    let want = gmclip_app::events::events_ts();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/src/gen/events.ts");
    if std::env::var_os("GMCLIP_GEN_WRITE").is_some() {
        std::fs::write(&path, &want).unwrap();
        eprintln!("wrote {}", path.display());
        return;
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "app/ui/src/gen/events.ts is out of date with the app's event types: run `cargo xt gen`"
    );
}
