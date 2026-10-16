use super::{Amount, currency_precision};
use crate::error::{Error, Result, require};
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{One, ToPrimitive, Zero};
use std::collections::BTreeMap;
use uuid::Uuid;

pub fn parse_rate(rate: &str) -> Result<BigRational> {
    require(
        !rate.is_empty() && rate.len() <= 100,
        "Rate must be a positive decimal of at most 100 characters",
    )?;
    let parts = rate.split('.').collect::<Vec<_>>();
    require(
        parts.len() <= 2
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
        "Rate must be an exact positive decimal, without exponent",
    )?;
    let numerator = parts
        .join("")
        .parse::<BigInt>()
        .map_err(|_| Error::Invalid("Invalid rate".into()))?;
    require(numerator > BigInt::zero(), "Rate must be positive")?;
    let denominator = BigInt::from(10).pow(parts.get(1).map_or(0, |p| p.len() as u32));
    Ok(BigRational::new(numerator, denominator))
}
pub fn convert(amount: Amount, from: &str, to: &str, rate: &str) -> Result<Amount> {
    let exact = BigRational::from_integer(BigInt::from(amount.value()))
        * parse_rate(rate)?
        * BigRational::new(
            BigInt::from(10).pow(currency_precision(to)? as u32),
            BigInt::from(10).pow(currency_precision(from)? as u32),
        );
    let negative = exact < BigRational::zero();
    let exact = if negative { -exact } else { exact };
    let mut rounded = exact.numer() / exact.denom();
    let remainder = exact.numer() % exact.denom();
    let twice = &remainder * 2;
    let odd = (&rounded % 2) != BigInt::zero();
    if twice > *exact.denom() || (twice == *exact.denom() && odd) {
        rounded += BigInt::one();
    }
    if negative {
        rounded = -rounded;
    }
    Amount::new(rounded.to_i128().ok_or(Error::Overflow)?)
}
/// Allocate the already-rounded component total before applying any category/tag filters.
pub fn apportion(total: Amount, allocations: &[(Uuid, Amount)]) -> Result<BTreeMap<Uuid, Amount>> {
    require(
        total >= Amount::ZERO,
        "Apportionment total must be nonnegative",
    )?;
    let denominator = allocations.iter().try_fold(Amount::ZERO, |a, (_, v)| {
        v.positive()?;
        a.checked_add(*v)
    })?;
    if denominator == Amount::ZERO {
        require(
            total == Amount::ZERO,
            "Cannot allocate a nonzero total to no allocations",
        )?;
        return Ok(BTreeMap::new());
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut portions = vec![];
    let mut assigned = Amount::ZERO;
    for (id, value) in allocations {
        require(seen.insert(*id), "Allocation IDs must be unique")?;
        let product = BigInt::from(total.value()) * BigInt::from(value.value());
        let divisor = BigInt::from(denominator.value());
        let floor = Amount::new((&product / &divisor).to_i128().ok_or(Error::Overflow)?)?;
        assigned = assigned.checked_add(floor)?;
        portions.push((*id, floor, product % divisor));
    }
    portions.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    let remaining = total.value() - assigned.value();
    require(
        remaining >= 0 && remaining <= portions.len() as i128,
        "Invalid remainder distribution",
    )?;
    for (_, amount, _) in portions.iter_mut().take(remaining as usize) {
        *amount = amount.checked_add(Amount::new(1)?)?;
    }
    Ok(portions.into_iter().map(|(id, a, _)| (id, a)).collect())
}
