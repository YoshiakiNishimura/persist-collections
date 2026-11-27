use crate::{CollectionShape, IdChange, IdRecord, IdRecords};
use std::io;

/// A collection's addressing domain, ordered traversal, and accepted changes.
pub trait Collection: CollectionShape {
    type Address: Copy;
    type Item;
    type Change;

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

/// Apply a change to a collection in memory.
pub fn apply<C: Collection>(collection: &mut C, change: C::Change) -> io::Result<()> {
    collection.apply(change)
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
            VecChange::Remove { at } if at < self.len() => {
                self.remove(at);
            }
            VecChange::Replace { at, value } => return replace_at(self, at, value),
            VecChange::Insert { at, .. } | VecChange::Remove { at } => {
                return Err(invalid_position(at));
            }
        }
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
