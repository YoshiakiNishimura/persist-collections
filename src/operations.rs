use crate::{CollectionShape, IdChange, IdRecord, IdRecords};
use std::io;

/// A collection's addressing domain, ordered traversal, and accepted changes.
pub trait Collection: CollectionShape {
    type Address: Copy;
    type Item;
    type Change: ApplyRule<Self>;

    /// Traverse entries in the order guaranteed by the collection.
    fn entries(&self) -> impl Iterator<Item = (Self::Address, &Self::Item)>;

    /// Resolve a single address.
    fn at(&self, address: Self::Address) -> Option<&Self::Item>;

    /// Apply one change allowed by this collection's contract.
    fn apply(&mut self, change: Self::Change) -> io::Result<()>;
}

/// A restriction over a collection's addresses and values.
pub trait Selection<C: Collection> {
    fn select(self, collection: &C) -> impl Iterator<Item = (C::Address, &C::Item)>;
}

/// Select every entry.
pub struct All;

/// Select zero or one entries at the given address.
pub struct One<K>(pub K);

/// Select entries for which the predicate accepts the address and value.
pub struct Matching<P>(pub P);

impl<C: Collection> Selection<C> for All {
    fn select(self, collection: &C) -> impl Iterator<Item = (C::Address, &C::Item)> {
        collection.entries()
    }
}

impl<C: Collection> Selection<C> for One<C::Address> {
    fn select(self, collection: &C) -> impl Iterator<Item = (C::Address, &C::Item)> {
        collection.at(self.0).map(|item| (self.0, item)).into_iter()
    }
}

impl<C, P> Selection<C> for Matching<P>
where
    C: Collection,
    P: FnMut(C::Address, &C::Item) -> bool,
{
    fn select(mut self, collection: &C) -> impl Iterator<Item = (C::Address, &C::Item)> {
        collection
            .entries()
            .filter(move |&(address, item)| (self.0)(address, item))
    }
}

/// Borrow entries selected from a collection, keeping their original addresses.
pub fn restrict<C: Collection>(
    collection: &C,
    selection: impl Selection<C>,
) -> impl Iterator<Item = (C::Address, &C::Item)> {
    selection.select(collection)
}

/// A rule that can be applied to collections meeting its requirements.
pub trait ApplyRule<C: Collection + ?Sized> {
    fn apply_to(self, collection: &mut C) -> io::Result<()>;
}

/// Apply a rule to an eligible collection in memory.
pub fn apply<C: Collection>(collection: &mut C, rule: impl ApplyRule<C>) -> io::Result<()> {
    rule.apply_to(collection)
}

/// Values can be replaced at an existing position, preserving all positions and length.
pub trait ReplacePosition: Collection<Address = usize> {
    fn replace_position(&mut self, at: usize, value: Self::Item) -> io::Result<()>;
}

/// A position can be removed and subsequent positions shift down by one.
pub trait RemovePosition: Collection<Address = usize> {
    fn remove_position(&mut self, at: usize) -> io::Result<()>;
}

/// Replace the value at a position while preserving the collection's domain.
pub struct ReplaceAt<T> {
    pub at: usize,
    pub value: T,
}

impl<C: ReplacePosition> ApplyRule<C> for ReplaceAt<C::Item> {
    fn apply_to(self, collection: &mut C) -> io::Result<()> {
        collection.replace_position(self.at, self.value)
    }
}

/// Remove an element from a sequence with shifting positions.
///
/// ```compile_fail
/// use persist_collections::{RemoveAt, apply};
/// let mut array = [1, 2, 3];
/// apply(&mut array, RemoveAt { at: 1 }).unwrap();
/// ```
pub struct RemoveAt {
    pub at: usize,
}

impl<C: RemovePosition> ApplyRule<C> for RemoveAt {
    fn apply_to(self, collection: &mut C) -> io::Result<()> {
        collection.remove_position(self.at)
    }
}

/// Changes to a variable-length positional sequence.
pub enum VecChange<T> {
    Push(T),
    Insert { at: usize, value: T },
    Remove { at: usize },
    Replace { at: usize, value: T },
}

/// A change that preserves the length of a fixed-size array.
pub enum ArrayChange<T> {
    Replace { at: usize, value: T },
}

impl<T> ApplyRule<Vec<T>> for VecChange<T> {
    fn apply_to(self, collection: &mut Vec<T>) -> io::Result<()> {
        Collection::apply(collection, self)
    }
}

impl<T, const N: usize> ApplyRule<[T; N]> for ArrayChange<T> {
    fn apply_to(self, collection: &mut [T; N]) -> io::Result<()> {
        Collection::apply(collection, self)
    }
}

impl ApplyRule<IdRecords> for IdChange<String> {
    fn apply_to(self, collection: &mut IdRecords) -> io::Result<()> {
        IdRecords::apply(collection, self)
    }
}

impl<T> Collection for Vec<T> {
    type Address = usize;
    type Item = T;
    type Change = VecChange<T>;

    fn entries(&self) -> impl Iterator<Item = (usize, &T)> {
        self.iter().enumerate()
    }

    fn at(&self, address: usize) -> Option<&T> {
        self.get(address)
    }

    fn apply(&mut self, change: Self::Change) -> io::Result<()> {
        match change {
            VecChange::Push(value) => self.push(value),
            VecChange::Insert { at, value } if at <= self.len() => self.insert(at, value),
            VecChange::Remove { at } => return apply(self, RemoveAt { at }),
            VecChange::Replace { at, value } => return apply(self, ReplaceAt { at, value }),
            VecChange::Insert { at, .. } => return Err(invalid_position(at)),
        }
        Ok(())
    }
}

impl<T> ReplacePosition for Vec<T> {
    fn replace_position(&mut self, at: usize, value: T) -> io::Result<()> {
        replace_at(self, at, value)
    }
}

impl<T> RemovePosition for Vec<T> {
    fn remove_position(&mut self, at: usize) -> io::Result<()> {
        if at >= self.len() {
            return Err(invalid_position(at));
        }
        self.remove(at);
        Ok(())
    }
}

impl<T, const N: usize> Collection for [T; N] {
    type Address = usize;
    type Item = T;
    type Change = ArrayChange<T>;

    fn entries(&self) -> impl Iterator<Item = (usize, &T)> {
        self.iter().enumerate()
    }

    fn at(&self, address: usize) -> Option<&T> {
        self.as_slice().get(address)
    }

    fn apply(&mut self, change: Self::Change) -> io::Result<()> {
        let ArrayChange::Replace { at, value } = change;
        apply(self, ReplaceAt { at, value })
    }
}

impl<T, const N: usize> ReplacePosition for [T; N] {
    fn replace_position(&mut self, at: usize, value: T) -> io::Result<()> {
        replace_at(self, at, value)
    }
}

impl Collection for IdRecords {
    type Address = u64;
    type Item = IdRecord;
    type Change = IdChange<String>;

    fn entries(&self) -> impl Iterator<Item = (u64, &IdRecord)> {
        self.restrict(..).iter().map(|record| (record.id(), record))
    }

    fn at(&self, id: u64) -> Option<&IdRecord> {
        self.find(id)
    }

    fn apply(&mut self, change: Self::Change) -> io::Result<()> {
        IdRecords::apply(self, change)
    }
}

fn replace_at<T>(values: &mut [T], at: usize, value: T) -> io::Result<()> {
    *values.get_mut(at).ok_or_else(|| invalid_position(at))? = value;
    Ok(())
}

fn invalid_position(at: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("position {at} is out of bounds"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_replacement_rule_preserves_the_domain_of_both_sequences() -> io::Result<()> {
        fn replace_second<C: ReplacePosition<Item = i32>>(values: &mut C) -> io::Result<()> {
            apply(values, ReplaceAt { at: 1, value: 99 })
        }

        let mut vector = vec![10, 20, 30];
        let mut array = [10, 20, 30];
        replace_second(&mut vector)?;
        replace_second(&mut array)?;
        assert_eq!(vector, array);
        assert_eq!(restrict(&vector, All).count(), 3);
        assert_eq!(restrict(&array, All).count(), 3);
        assert!(apply(&mut array, ReplaceAt { at: 3, value: 0 }).is_err());
        assert_eq!(array, [10, 99, 30]);

        apply(&mut vector, RemoveAt { at: 0 })?;
        assert_eq!(restrict(&vector, One(0)).next(), Some((0, &99)));
        assert_eq!(vector, [99, 30]);
        Ok(())
    }

    #[test]
    fn positional_restrictions_keep_addresses_and_updates_enforce_length() -> io::Result<()> {
        let mut values = vec![10, 20, 30];
        assert_eq!(restrict(&values, One(1)).next(), Some((1, &20)));
        assert_eq!(
            restrict(&values, Matching(|_, value: &i32| *value != 20)).collect::<Vec<_>>(),
            [(0, &10), (2, &30)]
        );
        assert!(apply(&mut values, VecChange::Remove { at: 3 }).is_err());
        apply(&mut values, VecChange::Remove { at: 1 })?;
        assert_eq!(
            restrict(&values, All).collect::<Vec<_>>(),
            [(0, &10), (1, &30)]
        );
        apply(&mut values, VecChange::Insert { at: 2, value: 40 })?;
        apply(&mut values, VecChange::Replace { at: 1, value: 35 })?;
        apply(&mut values, VecChange::Push(50))?;
        assert_eq!(values, [10, 35, 40, 50]);

        let mut slots = [10, 20, 20];
        assert!(restrict(&slots, One(3)).next().is_none());
        assert_eq!(
            restrict(&slots, Matching(|_, value: &i32| *value == 20)).collect::<Vec<_>>(),
            [(1, &20), (2, &20)]
        );
        assert!(apply(&mut slots, ArrayChange::Replace { at: 3, value: 0 }).is_err());
        apply(&mut slots, ArrayChange::Replace { at: 1, value: 99 })?;
        assert_eq!(restrict(&slots, All).count(), 3);
        assert_eq!(slots, [10, 99, 20]);
        assert_eq!(restrict(&[0_u8; 0], All).count(), 0);
        Ok(())
    }

    #[test]
    fn id_restrictions_preserve_stable_addresses() -> io::Result<()> {
        let mut records = IdRecords::empty();
        for text in ["first", "repeated", "repeated"] {
            let change = records.prepare_insert(text.into())?;
            apply(&mut records, change)?;
        }
        apply(&mut records, IdChange::Delete { id: 1 })?;
        assert!(restrict(&records, One(1)).next().is_none());
        let (id, record) = restrict(&records, One(3)).next().unwrap();
        assert_eq!((id, record.text()), (3, "repeated"));
        assert_eq!(
            restrict(&records, All)
                .map(|(id, _)| id)
                .collect::<Vec<_>>(),
            [2, 3]
        );
        let selected = restrict(
            &records,
            Matching(|id, record: &IdRecord| id < 3 && record.text() == "repeated"),
        );
        assert_eq!(selected.map(|(id, _)| id).collect::<Vec<_>>(), [2]);
        assert_eq!(records.next_id(), 4);
        Ok(())
    }
}
