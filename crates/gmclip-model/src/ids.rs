//! Typed ids (SPEC §4.3.2). ULIDs are minted by the project actor from its
//! deterministic services and stored in the forward op (R-MODEL-1); nothing here mints.

use core::fmt;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A ULID: 48-bit millisecond timestamp + 80 random bits, written as 26 Crockford
/// base-32 characters. gmclip never mints one here: the actor's deterministic services
/// do (R-MODEL-1), so this type only stores, orders, prints and parses.
#[derive(Copy, Clone, Eq, Ord, PartialEq, PartialOrd, Hash, Default)]
pub struct Ulid(pub u128);

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Why a string is not a ULID.
#[derive(Copy, Clone, Eq, PartialEq, Debug, thiserror::Error)]
pub enum UlidError {
    #[error("a ULID has 26 characters")]
    Length,
    #[error("a ULID uses Crockford base-32 characters")]
    Char,
    #[error("a ULID's first character is at most 7")]
    Overflow,
}

impl Ulid {
    /// `ms` must fit in 48 bits and `random` in 80; higher bits are dropped.
    pub const fn from_parts(ms: u64, random: u128) -> Self {
        debug_assert!(ms < (1 << 48), "ULID timestamp exceeds 48 bits");
        Self(((ms as u128) << 80) | (random & ((1 << 80) - 1)))
    }

    /// Milliseconds since the Unix epoch.
    pub const fn timestamp_ms(self) -> u64 {
        (self.0 >> 80) as u64
    }

    /// Parses Crockford base-32, case-insensitively and with `I`/`L` → 1, `O` → 0 as the
    /// Crockford spec says; [`Display`](fmt::Display) always prints the canonical form.
    pub fn parse(s: &str) -> Result<Self, UlidError> {
        let b = s.as_bytes();
        if b.len() != 26 {
            return Err(UlidError::Length);
        }
        let mut v: u128 = 0;
        for (i, &c) in b.iter().enumerate() {
            let d = match c.to_ascii_uppercase() {
                c @ b'0'..=b'9' => c - b'0',
                b'O' => 0,
                b'I' | b'L' => 1,
                c => CROCKFORD
                    .iter()
                    .position(|&x| x == c)
                    .and_then(|p| u8::try_from(p).ok())
                    .ok_or(UlidError::Char)?,
            };
            if i == 0 && d > 7 {
                return Err(UlidError::Overflow);
            }
            v = (v << 5) | u128::from(d);
        }
        Ok(Self(v))
    }
}

impl fmt::Display for Ulid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = [0u8; 26];
        for (i, o) in out.iter_mut().enumerate() {
            let shift = 5 * (25 - i);
            *o = CROCKFORD[((self.0 >> shift) & 0x1f) as usize];
        }
        f.write_str(core::str::from_utf8(&out).map_err(|_| fmt::Error)?)
    }
}

impl fmt::Debug for Ulid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ulid({self})")
    }
}

impl Serialize for Ulid {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Ulid {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        Ulid::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Ulid {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Ulid".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^[0-7][0-9A-HJKMNP-TV-Z]{25}$",
            "description": "ULID (26 Crockford base-32 characters)"
        })
    }
}

/// An entity revision. Every entity carries one (R-CC-1).
pub type Rev = u64;

/// The kind of an entity, e.g. `"part"`. One per `#[derive(Entity)]` type; parsing a
/// name looks it up in the closed kind list (`crate::kinds`), so an unknown kind never
/// deserialises.
#[derive(Copy, Clone, Eq, Ord, PartialEq, PartialOrd, Hash, Debug, Serialize)]
#[serde(transparent)]
pub struct EntityKind(pub &'static str);

impl<'de> Deserialize<'de> for EntityKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        crate::kinds::kind_by_name(&s)
            .ok_or_else(|| serde::de::Error::custom("unknown entity kind"))
    }
}

impl JsonSchema for EntityKind {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "EntityKind".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let names: Vec<&str> = crate::kinds::KINDS.iter().map(|k| k.kind().0).collect();
        schemars::json_schema!({ "type": "string", "enum": names })
    }
}

/// An untyped entity reference: kind plus ULID. Serialised as `{kind, id}`.
#[derive(
    Copy, Clone, Eq, Ord, PartialEq, PartialOrd, Hash, Debug, Serialize, Deserialize, JsonSchema,
)]
pub struct EntityId {
    pub kind: EntityKind,
    pub id: Ulid,
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind.0, self.id)
    }
}

/// A typed id. `PhantomData<fn() -> E>` keeps `Id<E>` `Send + Sync + Copy` for any `E`.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id<E>(pub Ulid, #[serde(skip)] pub PhantomData<fn() -> E>);

impl<E> JsonSchema for Id<E> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Ulid".into()
    }
    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        Ulid::json_schema(g)
    }
}

impl<E> Default for Id<E> {
    fn default() -> Self {
        Self::new(Ulid(0))
    }
}

impl<E: crate::Entity> Id<E> {
    /// The untyped reference.
    pub fn entity(self) -> EntityId {
        EntityId {
            kind: E::KIND,
            id: self.0,
        }
    }
}

impl<E> Id<E> {
    pub const fn new(id: Ulid) -> Self {
        Self(id, PhantomData)
    }
}

impl<E> Clone for Id<E> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<E> Copy for Id<E> {}
impl<E> PartialEq for Id<E> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl<E> Eq for Id<E> {}
impl<E> PartialOrd for Id<E> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<E> Ord for Id<E> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}
impl<E> Hash for Id<E> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}
impl<E> fmt::Debug for Id<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({})", self.0)
    }
}

macro_rules! ulid_newtype {
    ($($(#[$m:meta])* $name:ident),* $(,)?) => {$(
        $(#[$m])*
        #[derive(Copy, Clone, Eq, Ord, PartialEq, PartialOrd, Hash, Debug, Serialize, Deserialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(pub Ulid);
    )*};
}

ulid_newtype! {
    /// A committed transaction.
    TxnId,
    /// An agent run (SPEC §3.6).
    RunId,
    /// A visual event (SPEC §4.1.3; detected from M4).
    EventId,
}

/// A caller-supplied idempotency key (R-CMD-7).
#[derive(
    Clone, Eq, Ord, PartialEq, PartialOrd, Hash, Debug, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct RequestId(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    struct Part;

    #[test]
    fn ulid_round_trips_through_text() {
        let u = Ulid::from_parts(1_760_000_000_000, 0x1234_5678_9abc_def0_1234);
        let s = u.to_string();
        assert_eq!(s.len(), 26);
        assert_eq!(Ulid::parse(&s), Ok(u));
        assert_eq!(Ulid::parse(&s.to_lowercase()), Ok(u));
        assert_eq!(u.timestamp_ms(), 1_760_000_000_000);
        assert_eq!(Ulid(u128::MAX).to_string(), "7ZZZZZZZZZZZZZZZZZZZZZZZZZ");
        assert_eq!(
            Ulid::parse("80000000000000000000000000"),
            Err(UlidError::Overflow)
        );
        assert_eq!(
            Ulid::parse("0000000000000000000000000U"),
            Err(UlidError::Char)
        );
        assert_eq!(Ulid::parse("0"), Err(UlidError::Length));
    }

    #[test]
    fn typed_ids_are_copy_and_ordered_by_ulid() {
        let a: Id<Part> = Id::new(Ulid(1));
        let b = a;
        assert_eq!(a, b);
        assert!(Id::<Part>::new(Ulid(1)) < Id::<Part>::new(Ulid(2)));
    }
}
