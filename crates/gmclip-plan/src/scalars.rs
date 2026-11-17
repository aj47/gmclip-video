//! Constrained scalars from the schema's `$defs`: `id`, `sha256`, `rgb`, `rgba`, `even`,
//! `range`, and the `fps.num` enum. Each checks its own constraint on deserialise, so a
//! value of these types is always well formed, and each emits the same `$defs` schema.

use std::borrow::{Borrow, Cow};
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use crate::pattern::Pattern;

macro_rules! pattern_string {
    ($(#[$doc:meta])* $name:ident, $def:literal, $pattern:literal) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// The schema's `pattern` for this type.
            pub const PATTERN: &'static str = $pattern;

            /// Checks `s` against [`Self::PATTERN`].
            pub fn new(s: impl Into<String>) -> Result<Self, String> {
                let s = s.into();
                let ok = Pattern::parse(Self::PATTERN)
                    .map(|p| p.is_match(&s))
                    .unwrap_or(false);
                if ok {
                    Ok(Self(s))
                } else {
                    Err(format!("does not match {}", Self::PATTERN))
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Self::new(s).map_err(de::Error::custom)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                $def.into()
            }
            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({ "type": "string", "pattern": $pattern })
            }
        }
    };
}

pattern_string!(
    /// An id: `^[A-Za-z0-9_.:-]{1,96}$`.
    Id, "id", "^[A-Za-z0-9_.:-]{1,96}$"
);
pattern_string!(
    /// A layer role name (`layers[].role`): `^[a-z_]{1,32}$`. Which names v0 accepts is
    /// rule 5 ([`crate::validate::LAYER_ROLES`]).
    RoleName, "role", "^[a-z_]{1,32}$"
);
pattern_string!(
    /// A lowercase hex SHA-256.
    Sha256, "sha256", "^[0-9a-f]{64}$"
);
pattern_string!(
    /// `#RRGGBB`.
    Rgb, "rgb", "^#[0-9a-fA-F]{6}$"
);
pattern_string!(
    /// `#RRGGBBAA`.
    Rgba, "rgba", "^#[0-9a-fA-F]{8}$"
);

/// An even integer ≥ 2 (canvas sides).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(transparent)]
pub struct Even(u64);

impl Even {
    pub fn new(v: u64) -> Result<Self, String> {
        if v >= 2 && v.is_multiple_of(2) {
            Ok(Self(v))
        } else {
            Err("must be an even integer ≥ 2".into())
        }
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for Even {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(u64::deserialize(d)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for Even {
    fn schema_name() -> Cow<'static, str> {
        "even".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "integer", "minimum": 2, "multipleOf": 2 })
    }
}

/// `output.fps.num`: an integer rate (rule 4), or under `/1` an NTSC numerator over
/// 1001 (decision 0020).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(transparent)]
pub struct FpsNum(u64);

impl FpsNum {
    /// The integer rates (`den` 1) every version accepts.
    pub const ALLOWED: [u64; 6] = [24, 25, 30, 48, 50, 60];
    /// The NTSC numerators (`den` 1001, `/1` only): 23.976, 29.97, 59.94.
    pub const NTSC: [u64; 3] = [24000, 30000, 60000];

    pub fn new(v: u64) -> Result<Self, String> {
        if Self::ALLOWED.contains(&v) || Self::NTSC.contains(&v) {
            Ok(Self(v))
        } else {
            Err("must be one of 24, 25, 30, 48, 50, 60 (or 24000, 30000, 60000 over 1001)".into())
        }
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for FpsNum {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(u64::deserialize(d)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for FpsNum {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "FpsNum".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "enum": [24, 25, 30, 48, 50, 60, 24000, 30000, 60000] })
    }
}

/// `output.fps.den`: 1, or 1001 for an NTSC rate (`/1`, decision 0020).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(transparent)]
pub struct FpsDen(u64);

impl FpsDen {
    pub const ONE: Self = Self(1);
    pub const NTSC: Self = Self(1001);

    pub fn new(v: u64) -> Result<Self, String> {
        if v == 1 || v == 1001 {
            Ok(Self(v))
        } else {
            Err("must be 1 or 1001".into())
        }
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for FpsDen {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(u64::deserialize(d)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for FpsDen {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "FpsDen".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "enum": [1, 1001] })
    }
}

/// `[start, end)` integers. `start < end` is a validator rule (rule 1), not a type
/// invariant, so an invalid plan still deserialises and gets a path-precise error.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Range(pub [u64; 2]);

impl Range {
    pub fn start(self) -> u64 {
        self.0[0]
    }

    pub fn end(self) -> u64 {
        self.0[1]
    }

    /// `end − start`, or 0 for an invalid range.
    pub fn len(self) -> u64 {
        self.end().saturating_sub(self.start())
    }

    pub fn is_empty(self) -> bool {
        self.end() <= self.start()
    }
}

impl JsonSchema for Range {
    fn schema_name() -> Cow<'static, str> {
        "range".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "array",
            "prefixItems": [{ "type": "integer", "minimum": 0 }, { "type": "integer", "minimum": 1 }],
            "minItems": 2,
            "maxItems": 2
        })
    }
}

/// serde helpers for a field that may be absent, `null` or a value
/// (`Option<Option<T>>`: outer = present, inner = non-null).
pub mod nullable {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer>(
        v: &Option<Option<T>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match v {
            Some(inner) => inner.serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(d).map(Some)
    }
}
