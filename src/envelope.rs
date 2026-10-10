//! P1: the envelope validator. `contract/envelope.json` drives it: its
//! `required` and `optional` maps are read once, in declared order, and only
//! the type names are implemented here.

use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};
use serde_json::{Map, Value};
use std::fmt;
use std::sync::OnceLock;

const CONTRACT: &str = include_str!("../contract/envelope.json");

/// One parsed line. `fields` holds every top-level field exactly as parsed,
/// ts/source/kind and unknown fields included.
#[derive(Debug, Clone)]
pub struct Event {
    pub ts: String,
    pub source: String,
    pub kind: String,
    pub fields: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EnvelopeError {
    NotJson,
    NotObject,
    MissingField(String),
    WrongType(String),
    BadTimestamp(String),
    BadUuid(String),
    BadName(String),
    BadKind(String),
    BadOrigin(String),
}

impl EnvelopeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotJson => "not_json",
            Self::NotObject => "not_object",
            Self::MissingField(_) => "missing_field",
            Self::WrongType(_) => "wrong_type",
            Self::BadTimestamp(_) => "bad_timestamp",
            Self::BadUuid(_) => "bad_uuid",
            Self::BadName(_) => "bad_name",
            Self::BadKind(_) => "bad_kind",
            Self::BadOrigin(_) => "bad_origin",
        }
    }
}

impl fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self.code();
        match self {
            Self::NotJson => write!(f, "{code}: line is not valid JSON"),
            Self::NotObject => write!(f, "{code}: line is not a JSON object"),
            Self::MissingField(n) => write!(f, "{code}: field {n:?} is missing or null"),
            Self::WrongType(n) => write!(f, "{code}: field {n:?} has the wrong JSON type"),
            Self::BadTimestamp(n)
            | Self::BadUuid(n)
            | Self::BadName(n)
            | Self::BadKind(n)
            | Self::BadOrigin(n) => write!(f, "{code}: field {n:?} has a malformed value"),
        }
    }
}

impl std::error::Error for EnvelopeError {}

#[derive(Clone, Copy)]
enum Ty {
    String,
    Integer,
    Number,
    Object,
    Uuid,
    Timestamp,
    Name,
    Kind,
    Origin,
}

/// A JSON object read as (key, value) pairs in document order. serde_json's
/// Map sorts keys without its preserve_order feature, and the declared order
/// decides which error is reported first.
struct Ordered(Vec<(String, String)>);

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Ordered;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map of field name to type name")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Ordered, A::Error> {
                let mut out = Vec::new();
                while let Some(entry) = m.next_entry()? {
                    out.push(entry);
                }
                Ok(Ordered(out))
            }
        }
        d.deserialize_map(V)
    }
}

#[derive(Deserialize)]
struct Raw {
    required: Ordered,
    optional: Ordered,
}

struct Contract {
    required: Vec<(String, Ty)>,
    optional: Vec<(String, Ty)>,
}

fn contract() -> &'static Contract {
    static C: OnceLock<Contract> = OnceLock::new();
    C.get_or_init(|| {
        let raw: Raw = serde_json::from_str(CONTRACT).expect("envelope.json is not valid");
        let typed = |o: Ordered| {
            o.0.into_iter()
                .map(|(field, ty)| {
                    let t = match ty.as_str() {
                        "string" => Ty::String,
                        "integer" => Ty::Integer,
                        "number" => Ty::Number,
                        "object" => Ty::Object,
                        "uuid" => Ty::Uuid,
                        "timestamp" => Ty::Timestamp,
                        "name" => Ty::Name,
                        "kind" => Ty::Kind,
                        "origin" => Ty::Origin,
                        _ => panic!("envelope.json: unknown type {ty:?} for field {field:?}"),
                    };
                    (field, t)
                })
                .collect()
        };
        Contract {
            required: typed(raw.required),
            optional: typed(raw.optional),
        }
    })
}

fn check(name: &str, ty: Ty, v: &Value) -> Result<(), EnvelopeError> {
    let wrong = || EnvelopeError::WrongType(name.to_owned());
    let fmt_check = |valid: fn(&str) -> bool, err: fn(String) -> EnvelopeError| {
        let s = v.as_str().ok_or_else(wrong)?;
        if valid(s) {
            Ok(())
        } else {
            Err(err(name.to_owned()))
        }
    };
    match ty {
        Ty::String => v.is_string().then_some(()).ok_or_else(wrong),
        Ty::Integer => v.as_u64().map(drop).ok_or_else(wrong),
        Ty::Number => v.as_f64().filter(|n| *n >= 0.0).map(drop).ok_or_else(wrong),
        Ty::Object => v.is_object().then_some(()).ok_or_else(wrong),
        Ty::Uuid => fmt_check(is_uuid, EnvelopeError::BadUuid),
        Ty::Timestamp => fmt_check(is_timestamp, EnvelopeError::BadTimestamp),
        Ty::Name => fmt_check(is_name, EnvelopeError::BadName),
        Ty::Kind => fmt_check(is_kind, EnvelopeError::BadKind),
        Ty::Origin => fmt_check(
            |s| matches!(s, "operator" | "agent" | "external"),
            EnvelopeError::BadOrigin,
        ),
    }
}

/// Validate one line against the envelope. Fields are checked in the order
/// envelope.json declares them, required map first; the first error wins.
pub fn parse_line(line: &str) -> Result<Event, EnvelopeError> {
    let value: Value = serde_json::from_str(line).map_err(|_| EnvelopeError::NotJson)?;
    let Value::Object(fields) = value else {
        return Err(EnvelopeError::NotObject);
    };
    let c = contract();
    for (name, ty) in &c.required {
        match fields.get(name) {
            None | Some(Value::Null) => return Err(EnvelopeError::MissingField(name.clone())),
            Some(v) => check(name, *ty, v)?,
        }
    }
    for (name, ty) in &c.optional {
        if let Some(v) = fields.get(name).filter(|v| !v.is_null()) {
            check(name, *ty, v)?;
        }
    }
    // ponytail: empty if envelope.json ever stops requiring one of these.
    let s = |k: &str| {
        fields
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Ok(Event {
        ts: s("ts"),
        source: s("source"),
        kind: s("kind"),
        fields,
    })
}

/// `YYYY-MM-DDTHH:MM:SS[.f{1,9}](Z|+hh:mm|-hh:mm)`.
pub fn is_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    let num = |i: usize, n: usize| -> Option<u32> {
        b.get(i..i + n)?.iter().try_fold(0, |acc, &c| {
            c.is_ascii_digit().then(|| acc * 10 + u32::from(c - b'0'))
        })
    };
    let at = |i: usize, c: u8| b.get(i) == Some(&c);
    let within = |i, n, lo, hi| num(i, n).is_some_and(|v| (lo..=hi).contains(&v));
    let date_time = num(0, 4).is_some()
        && at(4, b'-')
        && within(5, 2, 1, 12)
        && at(7, b'-')
        && within(8, 2, 1, 31)
        && at(10, b'T')
        && within(11, 2, 0, 23)
        && at(13, b':')
        && within(14, 2, 0, 59)
        && at(16, b':')
        && within(17, 2, 0, 59);
    if !date_time {
        return false;
    }
    let mut i = 19;
    if at(i, b'.') {
        let n = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        if !(1..=9).contains(&n) {
            return false;
        }
        i += 1 + n;
    }
    match b.get(i) {
        Some(b'Z') => i + 1 == b.len(),
        Some(b'+' | b'-') => {
            i + 6 == b.len()
                && within(i + 1, 2, 0, 23)
                && at(i + 3, b':')
                && within(i + 4, 2, 0, 59)
        }
        _ => false,
    }
}

/// 8-4-4-4-12 hex digits, either case.
pub fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn is_segment(s: &str, extra: &[u8]) -> bool {
    let mut b = s.bytes();
    b.next().is_some_and(|c| c.is_ascii_lowercase())
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || extra.contains(&c))
}

/// `^[a-z][a-z0-9_-]*$`
pub fn is_name(s: &str) -> bool {
    is_segment(s, b"_-")
}

/// `^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$`
pub fn is_kind(s: &str) -> bool {
    s.contains('.') && s.split('.').all(|seg| is_segment(seg, b"_"))
}
