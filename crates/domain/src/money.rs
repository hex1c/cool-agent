use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoneyError {
    InvalidCurrency,
    CurrencyMismatch {
        expected: CurrencyCode,
        actual: CurrencyCode,
    },
    Overflow,
}

impl Display for MoneyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCurrency => {
                formatter.write_str("currency must be a three-letter ISO code")
            }
            Self::CurrencyMismatch { expected, actual } => {
                write!(
                    formatter,
                    "currency mismatch: expected {expected}, got {actual}"
                )
            }
            Self::Overflow => formatter.write_str("money operation overflowed"),
        }
    }
}

impl std::error::Error for MoneyError {}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct CurrencyCode(String);

impl CurrencyCode {
    pub fn new(value: impl AsRef<str>) -> Result<Self, MoneyError> {
        let value = value.as_ref();
        if value.len() != 3 || !value.bytes().all(|byte| byte.is_ascii_uppercase()) {
            return Err(MoneyError::InvalidCurrency);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn inr() -> Self {
        Self(String::from("INR"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for CurrencyCode {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl<'de> serde::Deserialize<'de> for CurrencyCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Money {
    pub amount_micro: i64,
    pub currency: CurrencyCode,
}

impl Money {
    pub const fn zero(currency: CurrencyCode) -> Self {
        Self {
            amount_micro: 0,
            currency,
        }
    }

    pub fn new(amount_micro: i64, currency: CurrencyCode) -> Self {
        Self {
            amount_micro,
            currency,
        }
    }

    pub fn checked_add(&self, other: &Self) -> Result<Self, MoneyError> {
        self.ensure_same_currency(other)?;
        let amount_micro = self
            .amount_micro
            .checked_add(other.amount_micro)
            .ok_or(MoneyError::Overflow)?;
        Ok(Self::new(amount_micro, self.currency.clone()))
    }

    pub fn checked_sub(&self, other: &Self) -> Result<Self, MoneyError> {
        self.ensure_same_currency(other)?;
        let amount_micro = self
            .amount_micro
            .checked_sub(other.amount_micro)
            .ok_or(MoneyError::Overflow)?;
        Ok(Self::new(amount_micro, self.currency.clone()))
    }

    fn ensure_same_currency(&self, other: &Self) -> Result<(), MoneyError> {
        if self.currency != other.currency {
            return Err(MoneyError::CurrencyMismatch {
                expected: self.currency.clone(),
                actual: other.currency.clone(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{CurrencyCode, Money, MoneyError};

    #[test]
    fn money_identity_routing_uses_valid_currency_and_checked_arithmetic() {
        let inr = CurrencyCode::inr();
        let first = Money::new(1_000_000, inr.clone());
        let second = Money::new(250_000, inr.clone());

        assert_eq!(
            first.checked_add(&second).map(|money| money.amount_micro),
            Ok(1_250_000)
        );
        assert_eq!(
            first.checked_sub(&second).map(|money| money.amount_micro),
            Ok(750_000)
        );
        assert_eq!(inr.as_str(), "INR");
    }

    #[test]
    fn money_rejects_invalid_currency_mismatch_and_overflow() {
        assert_eq!(CurrencyCode::new("inr"), Err(MoneyError::InvalidCurrency));

        let mismatch = CurrencyCode::new("USD").map(|usd| {
            let first = Money::new(1, CurrencyCode::inr());
            let other = Money::new(1, usd);
            first.checked_add(&other)
        });
        assert!(matches!(
            mismatch,
            Ok(Err(MoneyError::CurrencyMismatch { .. }))
        ));

        let max = Money::new(i64::MAX, CurrencyCode::inr());
        assert_eq!(
            max.checked_add(&Money::new(1, CurrencyCode::inr())),
            Err(MoneyError::Overflow)
        );
    }

    #[test]
    fn currency_code_deserialization_rejects_invalid() {
        let lowercase: Result<CurrencyCode, _> = serde_json::from_str("\"inr\"");
        assert!(lowercase.is_err());

        let too_short: Result<CurrencyCode, _> = serde_json::from_str("\"US\"");
        assert!(too_short.is_err());

        let too_long: Result<CurrencyCode, _> = serde_json::from_str("\"USDD\"");
        assert!(too_long.is_err());
    }

    #[test]
    fn currency_code_deserialization_accepts_valid() {
        let deser: Result<CurrencyCode, _> = serde_json::from_str("\"USD\"");
        let ctor = CurrencyCode::new("USD");
        assert!(matches!((&deser, &ctor), (Ok(d), Ok(c)) if d == c));
    }
}
