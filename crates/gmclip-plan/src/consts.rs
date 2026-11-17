//! Schema `const` values as zero-sized types: a field typed `H264` can only ever hold
//! `"h264"`, and its derived schema says `{"const": "h264"}`.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};

macro_rules! str_const {
    ($(#[$doc:meta])* $name:ident = $value:literal) => {
        $(#[$doc])*
        #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
        pub struct $name;

        impl $name {
            /// The one value this type serialises to.
            pub const VALUE: &'static str = $value;
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str($value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct V;
                impl Visitor<'_> for V {
                    type Value = $name;
                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        write!(f, "the string {:?}", $value)
                    }
                    fn visit_str<E: de::Error>(self, v: &str) -> Result<$name, E> {
                        if v == $value {
                            Ok($name)
                        } else {
                            Err(E::invalid_value(de::Unexpected::Other("another string"), &self))
                        }
                    }
                }
                d.deserialize_str(V)
            }
        }

        impl JsonSchema for $name {
            fn inline_schema() -> bool {
                true
            }
            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }
            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({ "type": "string", "const": $value })
            }
        }
    };
}

str_const!(
    /// `schema`: `"gmclip.renderplan/0"`.
    SchemaV0 = "gmclip.renderplan/0"
);
/// The `schema` of a plan: `/0`, or `/1` (decision 0014: image and shape events, the
/// blurred-source background, caption animation). A plan that uses none of `/1`'s
/// additions is written `/0`, so its bytes and `render_sig` do not change.
#[derive(
    Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
pub enum SchemaVersion {
    #[default]
    #[serde(rename = "gmclip.renderplan/0")]
    V0,
    #[serde(rename = "gmclip.renderplan/1")]
    V1,
}

impl SchemaVersion {
    pub fn as_str(self) -> &'static str {
        match self {
            SchemaVersion::V0 => "gmclip.renderplan/0",
            SchemaVersion::V1 => "gmclip.renderplan/1",
        }
    }
}

impl JsonSchema for SchemaVersion {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "SchemaVersion".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "enum": ["gmclip.renderplan/0", "gmclip.renderplan/1"] })
    }
}

str_const!(H264 = "h264");
str_const!(AacLc = "aac_lc");
str_const!(Bt709 = "bt709");
str_const!(Vbr = "vbr");
str_const!(Lanczos3 = "lanczos3");
str_const!(LibassBs3 = "libass_bs3");
str_const!(LinearDb = "linear_db");
str_const!(RoundJoin = "round");
str_const!(
    /// `layer.matte.kind`: `"roto"` (decision 0013).
    Roto = "roto"
);

/// An integer `const` (`{"const": N}`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct ConstU64<const N: u64>;

impl<const N: u64> ConstU64<N> {
    pub const VALUE: u64 = N;
}

impl<const N: u64> Serialize for ConstU64<N> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(N)
    }
}

impl<'de, const N: u64> Deserialize<'de> for ConstU64<N> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = u64::deserialize(d)?;
        if v == N {
            Ok(Self)
        } else {
            Err(de::Error::custom(format_args!("expected the integer {N}")))
        }
    }
}

impl<const N: u64> JsonSchema for ConstU64<N> {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        format!("Const{N}").into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "integer", "const": N })
    }
}

/// The boolean `const false` (`container.edit_lists`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct ConstFalse;

impl Serialize for ConstFalse {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bool(false)
    }
}

impl<'de> Deserialize<'de> for ConstFalse {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        if bool::deserialize(d)? {
            Err(de::Error::custom("expected false"))
        } else {
            Ok(Self)
        }
    }
}

impl JsonSchema for ConstFalse {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "ConstFalse".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "boolean", "const": false })
    }
}
