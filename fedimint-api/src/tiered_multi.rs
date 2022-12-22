use std::cmp::min;
use std::collections::BTreeMap;
use std::iter::FromIterator;
use std::marker::PhantomData;

use fedimint_api::encoding::{Decodable, DecodeError, Encodable};
use serde::{Deserialize, Serialize};

use crate::module::registry::ModuleDecoderRegistry;
use crate::tiered::InvalidAmountTierError;
use crate::{Amount, Tiered};

/// Represents coins of different denominations.
///
/// **Attention:** care has to be taken when constructing this to avoid overflow when calculating
/// the total amount represented. As it is prudent to limit both the maximum coin amount and maximum
/// coin count per transaction this shouldn't be a problem in practice though.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize)]
pub struct TieredMulti<T>(BTreeMap<Amount, Vec<T>>);

impl<T> TieredMulti<T> {
    pub fn new(map: BTreeMap<Amount, Vec<T>>) -> Self {
        TieredMulti(map)
    }

    pub fn total_amount(&self) -> Amount {
        let milli_sat = self
            .0
            .iter()
            .map(|(tier, coins)| tier.msats * (coins.len() as u64))
            .sum();
        Amount { msats: milli_sat }
    }

    pub fn item_count(&self) -> usize {
        self.0.values().map(|coins| coins.len()).sum()
    }

    pub fn tier_count(&self) -> usize {
        self.0.len()
    }

    pub fn tiers(&self) -> impl Iterator<Item = &Amount> {
        self.0.keys()
    }

    pub fn is_empty(&self) -> bool {
        self.item_count() == 0
    }

    pub fn map<F, N, E>(self, f: F) -> Result<TieredMulti<N>, E>
    where
        F: Fn(Amount, T) -> Result<N, E>,
    {
        let res = self
            .0
            .into_iter()
            .map(|(amt, coins)| -> Result<_, E> {
                let coins = coins
                    .into_iter()
                    .map(|coin| f(amt, coin))
                    .collect::<Result<Vec<_>, E>>()?;
                Ok((amt, coins))
            })
            .collect::<Result<BTreeMap<Amount, Vec<N>>, E>>()?;

        Ok(TieredMulti(res))
    }

    pub fn structural_eq<O>(&self, other: &TieredMulti<O>) -> bool {
        let tier_eq = self.0.keys().eq(other.0.keys());
        let per_tier_eq = self
            .0
            .values()
            .zip(other.0.values())
            .all(|(c1, c2)| c1.len() == c2.len());

        tier_eq && per_tier_eq
    }

    pub fn iter_tiers(&self) -> impl Iterator<Item = (&Amount, &Vec<T>)> {
        self.0.iter()
    }

    // Note: order of the elements is important here: from lowest tiers to highest, then in order of elements in the Vec
    pub fn iter_items(&self) -> impl Iterator<Item = (Amount, &T)> + DoubleEndedIterator {
        self.0
            .iter()
            .flat_map(|(amt, coins)| coins.iter().map(move |c| (*amt, c)))
    }
    // Note: order of the elements is important here: from lowest tiers to highest, then in order of elements in the Vec
    pub fn into_iter_items(self) -> impl Iterator<Item = (Amount, T)> + DoubleEndedIterator {
        self.0
            .into_iter()
            .flat_map(|(amt, coins)| coins.into_iter().map(move |c| (amt, c)))
    }

    /// Returns the max number of notes for any given denomination tier
    pub fn max_tier_len(&self) -> usize {
        self.0.values().map(|notes| notes.len()).max().unwrap_or(0)
    }

    pub fn check_tiers<K>(&self, keys: &Tiered<K>) -> Result<(), InvalidAmountTierError> {
        match self.0.keys().find(|&amt| keys.get(*amt).is_none()) {
            Some(amt) => Err(InvalidAmountTierError(*amt)),
            None => Ok(()),
        }
    }

    pub fn get(&self, amt: Amount) -> Option<&Vec<T>> {
        self.0.get(&amt)
    }

    // TODO: Get rid of it. It might be used to break useful invariants (like making sure there are no empty `Vec`s after removal)
    pub fn get_mut(&mut self, amt: Amount) -> Option<&mut Vec<T>> {
        self.0.get_mut(&amt)
    }
}

impl<C> TieredMulti<C>
where
    C: Clone,
{
    /// Select coins with total amount of *at least* `amount`. If more than requested amount of coins
    /// are returned it was because exact change couldn't be made, and the next smallest amount will be
    /// returned.
    ///
    /// The caller can request change from the federation.
    // TODO: move somewhere else?
    pub fn select_coins(&self, amount: Amount) -> Option<TieredMulti<C>> {
        if amount > self.total_amount() {
            return None;
        }

        let mut remaining = self.total_amount();

        let coins = self
            .iter_items()
            .rev()
            .filter_map(|(coin_amount, coin)| {
                if amount <= remaining - coin_amount {
                    remaining -= coin_amount;
                    None
                } else {
                    Some((coin_amount, (*coin).clone()))
                }
            })
            .collect::<TieredMulti<C>>();

        Some(coins)
    }
}

impl TieredMulti<()> {
    /// Determines the denominations to use when representing an amount
    ///
    /// Algorithm tries to leave the user with a target number of `denomination_sets` starting
    /// at the lowest denomination.  `self` gives the denominations that the user already has.
    pub fn represent_amount<K, V>(
        amount: Amount,
        current_denominations: &TieredMulti<V>,
        tiers: &Tiered<K>,
        denomination_sets: u16,
    ) -> Tiered<usize> {
        let mut remaining_amount = amount;
        let mut denominations: Tiered<usize> = Default::default();

        // try to hit the target `denomination_sets`
        for tier in tiers.tiers() {
            let notes = current_denominations
                .get(*tier)
                .map(|v| v.len())
                .unwrap_or(0);
            let missing_notes = (denomination_sets as u64).saturating_sub(notes as u64);
            let possible_notes = remaining_amount / *tier;

            let add_notes = min(possible_notes, missing_notes);
            *denominations.get_mut_or_default(*tier) = add_notes as usize;
            remaining_amount -= *tier * add_notes;
        }

        // if there is a remaining amount, add denominations with a greedy algorithm
        for tier in tiers.tiers().rev() {
            let res = remaining_amount / *tier;
            remaining_amount %= *tier;
            *denominations.get_mut_or_default(*tier) += res as usize;
        }

        let represented: u64 = denominations
            .iter()
            .map(|(k, v)| k.msats * (*v as u64))
            .sum();
        assert_eq!(represented, amount.msats);
        denominations
    }
}

impl<C> FromIterator<(Amount, C)> for TieredMulti<C> {
    fn from_iter<T: IntoIterator<Item = (Amount, C)>>(iter: T) -> Self {
        let mut res = TieredMulti::default();
        res.extend(iter);
        res
    }
}

impl<C> IntoIterator for TieredMulti<C>
where
    C: 'static + Send,
{
    type Item = (Amount, C);
    type IntoIter = Box<dyn Iterator<Item = (Amount, C)> + Send>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(
            self.0
                .into_iter()
                .flat_map(|(amt, coins)| coins.into_iter().map(move |c| (amt, c))),
        )
    }
}

impl<C> Default for TieredMulti<C> {
    fn default() -> Self {
        TieredMulti(BTreeMap::default())
    }
}

impl<C> Extend<(Amount, C)> for TieredMulti<C> {
    fn extend<T: IntoIterator<Item = (Amount, C)>>(&mut self, iter: T) {
        for (amount, coin) in iter {
            self.0.entry(amount).or_default().push(coin)
        }
    }
}

impl<C> Encodable for TieredMulti<C>
where
    C: Encodable,
{
    fn consensus_encode<W: std::io::Write>(&self, writer: &mut W) -> Result<usize, std::io::Error> {
        self.0.consensus_encode(writer)
    }
}

impl<C> Decodable for TieredMulti<C>
where
    C: Decodable,
{
    fn consensus_decode<D: std::io::Read>(
        d: &mut D,
        modules: &ModuleDecoderRegistry,
    ) -> Result<Self, DecodeError> {
        Ok(TieredMulti(BTreeMap::consensus_decode(d, modules)?))
    }
}

pub struct TieredMultiZip<'a, I, T>
where
    I: 'a,
{
    iters: Vec<I>,
    _pd: PhantomData<&'a T>,
}

impl<'a, I, C> TieredMultiZip<'a, I, C> {
    /// Creates a new MultiZip Iterator from `Coins` iterators. These have to be checked for
    /// structural equality! There also has to be at least one iterator in the `iter` vector.
    pub fn new(iters: Vec<I>) -> Self {
        assert!(!iters.is_empty());

        TieredMultiZip {
            iters,
            _pd: Default::default(),
        }
    }
}

impl<'a, I, C> Iterator for TieredMultiZip<'a, I, C>
where
    I: Iterator<Item = (Amount, C)>,
{
    type Item = (Amount, Vec<C>);

    fn next(&mut self) -> Option<Self::Item> {
        let mut coins = Vec::with_capacity(self.iters.len());
        let mut amount = None;
        for iter in self.iters.iter_mut() {
            match iter.next() {
                Some((amt, coin)) => {
                    if let Some(amount) = amount {
                        // This may fail if coins weren't tested for structural equality
                        assert_eq!(amount, amt);
                    } else {
                        amount = Some(amt);
                    }
                    coins.push(coin);
                }
                None => return None,
            }
        }

        // This should always hold as long as this impl is correct
        assert_eq!(coins.len(), self.iters.len());

        Some((
            amount.expect("The multi zip must contain at least one iterator"),
            coins,
        ))
    }
}

#[cfg(test)]
mod test {
    use fedimint_api::Amount;

    use crate::{Tiered, TieredMulti};

    #[test]
    fn represent_amount_targets_denomination_sets() {
        let starting = coins(vec![
            (Amount::from_sats(1), 1),
            (Amount::from_sats(2), 3),
            (Amount::from_sats(3), 2),
        ]);
        let tiers = tiers(vec![1, 2, 3, 4]);

        // target 3 tiers will fill out the 1 and 3 denominations
        assert_eq!(
            TieredMulti::represent_amount(Amount::from_sats(6), &starting, &tiers, 3),
            denominations(vec![
                (Amount::from_sats(1), 3),
                (Amount::from_sats(2), 0),
                (Amount::from_sats(3), 1),
                (Amount::from_sats(4), 0)
            ])
        );

        // target 2 tiers will fill out the 1 and 4 denominations
        assert_eq!(
            TieredMulti::represent_amount(Amount::from_sats(6), &starting, &tiers, 2),
            denominations(vec![
                (Amount::from_sats(1), 2),
                (Amount::from_sats(2), 0),
                (Amount::from_sats(3), 0),
                (Amount::from_sats(4), 1)
            ])
        );
    }

    #[test]
    fn select_coins_returns_exact_amount() {
        let starting = coins(vec![
            (Amount::from_sats(1), 5),
            (Amount::from_sats(5), 5),
            (Amount::from_sats(20), 5),
        ]);

        assert_eq!(
            starting.select_coins(Amount::from_sats(7)),
            Some(coins(vec![
                (Amount::from_sats(1), 2),
                (Amount::from_sats(5), 1)
            ]))
        );
    }

    #[test]
    fn select_coins_uses_smaller_denominations() {
        let starting = coins(vec![(Amount::from_sats(5), 5), (Amount::from_sats(20), 5)]);

        assert_eq!(
            starting.select_coins(Amount::from_sats(7)),
            Some(coins(vec![(Amount::from_sats(5), 2)]))
        );
    }

    #[test]
    fn select_coins_returns_none_if_amount_is_too_large() {
        let starting = coins(vec![(Amount::from_sats(10), 1)]);

        assert_eq!(starting.select_coins(Amount::from_sats(100)), None);
    }

    fn coins(coins: Vec<(Amount, usize)>) -> TieredMulti<usize> {
        coins
            .into_iter()
            .flat_map(|(amount, number)| vec![(amount, 0_usize); number])
            .collect()
    }

    fn tiers(tiers: Vec<u64>) -> Tiered<()> {
        tiers
            .into_iter()
            .map(|tier| (Amount::from_sats(tier), ()))
            .collect()
    }

    fn denominations(denominations: Vec<(Amount, usize)>) -> Tiered<usize> {
        denominations.into_iter().collect()
    }
}
