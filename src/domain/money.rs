use crate::error::{Error, Result, require};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};

/// Integer minor units. JSON numbers and fractional values are never accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, JsonSchema)]
#[schemars(with = "String")]
pub struct Amount(i128);

impl Amount {
    pub const MAX: i128 = 10_i128.pow(38) - 1;
    pub const ZERO: Self = Self(0);
    pub fn new(value: i128) -> Result<Self> {
        if (-Self::MAX..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(Error::Overflow)
        }
    }
    pub fn value(self) -> i128 {
        self.0
    }
    pub fn checked_add(self, rhs: Self) -> Result<Self> {
        Self::new(self.0.checked_add(rhs.0).ok_or(Error::Overflow)?)
    }
    pub fn checked_neg(self) -> Result<Self> {
        Self::new(-self.0)
    }
    pub fn positive(self) -> Result<Self> {
        require(self.0 > 0, "Amount must be greater than zero")?;
        Ok(self)
    }
}
impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl FromStr for Amount {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        let digits = value.strip_prefix('-').unwrap_or(value);
        require(
            !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
            "Amount must be an integer string",
        )?;
        Self::new(value.parse().map_err(|_| Error::Overflow)?)
    }
}
impl Serialize for Amount {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}
impl<'de> Deserialize<'de> for Amount {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Supported currency precision table, version sumi-currencies-1 (explicit subset).
pub const CURRENCIES: &[(&str, u8)] = &[
    ("CNY", 2),
    ("USD", 2),
    ("EUR", 2),
    ("GBP", 2),
    ("HKD", 2),
    ("SGD", 2),
    ("JPY", 0),
    ("KWD", 3),
    ("BHD", 3),
    ("KRW", 0),
    ("CHF", 2),
    ("AUD", 2),
    ("CAD", 2),
    ("NZD", 2),
    ("TWD", 2),
    ("THB", 2),
    ("MYR", 2),
    ("INR", 2),
];

pub fn currency_precision(currency: &str) -> Result<u8> {
    CURRENCIES
        .iter()
        .find(|(code, _)| *code == currency)
        .map(|(_, p)| *p)
        .ok_or_else(|| Error::Invalid(format!("Unsupported currency: {currency}")))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Money {
    pub currency: String,
    pub amount_minor: Amount,
}
impl Money {
    pub fn validate(&self) -> Result<()> {
        currency_precision(&self.currency)?;
        Ok(())
    }
}
