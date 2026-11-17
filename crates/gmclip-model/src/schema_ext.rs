//! JSON Schemas for types defined in `time.rs` (Lane A's file), kept here so the
//! entity schemas build without editing it. If `time.rs` later derives `JsonSchema`
//! itself, delete the matching impl here.

use schemars::JsonSchema;

use crate::time::{Flicks, Rational};

impl JsonSchema for Flicks {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Flicks".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "integer",
            "format": "int64",
            "description": "Time in flicks (1/705 600 000 s)"
        })
    }
}

impl JsonSchema for Rational {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Rational".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "properties": {
                "num": { "type": "integer", "format": "int64" },
                "den": { "type": "integer", "format": "uint32", "minimum": 1 }
            },
            "required": ["num", "den"]
        })
    }
}
