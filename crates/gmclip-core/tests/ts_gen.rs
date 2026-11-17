//! A2.01's CI check (TS ≡ JSON Schema): `app/ui/src/gen/commands.ts` is what the command
//! registry's schemas give. Fails while they differ; `cargo xt gen` (this test with
//! `GMCLIP_GEN_WRITE=1`) writes the file again.

use std::path::Path;

#[test]
fn the_ui_types_match_the_command_schemas() {
    let want = gmclip_core::tsgen::commands_ts();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/ui/src/gen/commands.ts");
    if std::env::var_os("GMCLIP_GEN_WRITE").is_some() {
        std::fs::write(&path, &want).unwrap();
        eprintln!("wrote {}", path.display());
        return;
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "app/ui/src/gen/commands.ts is out of date with the command schemas: run `cargo xt gen`"
    );
}
