use crate::IdRecords;

/// The stable name and version of a collection's logical contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Shape {
    pub family: &'static str,
    pub version: u32,
    pub addressing: Addressing,
    pub length: Length,
    pub order: IterationOrder,
    pub allows_duplicate_values: bool,
}

/// How an element can be addressed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Addressing {
    /// An element's position in the sequence. Edits may shift positions.
    Position,
    /// An explicit unique ID that is stable when other records are removed.
    UniqueId { allocation: IdAllocation },
}

/// How explicit IDs are assigned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum IdAllocation {
    /// A successful insertion advances the counter; deletion keeps it.
    /// Restoring an older snapshot also restores its counter.
    Monotonic,
    Unspecified,
}

/// Which lengths are allowed for this collection type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Length {
    Fixed(usize),
    Variable,
}

/// The guaranteed order when enumerating the whole collection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum IterationOrder {
    Position,
    AscendingId,
    Unspecified,
}

mod sealed {
    pub trait Sealed {}

    impl<T> Sealed for Vec<T> {}
    impl<T, const N: usize> Sealed for [T; N] {}
    impl Sealed for super::IdRecords {}
}

/// Static metadata for supported collection types, independent of their codecs.
///
/// A shipped family and version keep the same meaning. Type parameters such
/// as an array's length are represented separately in the shape.
pub trait CollectionShape: sealed::Sealed {
    const SHAPE: Shape;
}

impl<T> CollectionShape for Vec<T> {
    const SHAPE: Shape = Shape {
        family: "vec",
        version: 1,
        addressing: Addressing::Position,
        length: Length::Variable,
        order: IterationOrder::Position,
        allows_duplicate_values: true,
    };
}

impl<T, const N: usize> CollectionShape for [T; N] {
    const SHAPE: Shape = Shape {
        family: "array",
        version: 1,
        addressing: Addressing::Position,
        length: Length::Fixed(N),
        order: IterationOrder::Position,
        allows_duplicate_values: true,
    };
}

impl CollectionShape for IdRecords {
    const SHAPE: Shape = Shape {
        family: "id_records",
        version: 1,
        addressing: Addressing::UniqueId {
            allocation: IdAllocation::Monotonic,
        },
        length: Length::Variable,
        order: IterationOrder::AscendingId,
        allows_duplicate_values: true,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IdChange;

    #[test]
    fn built_in_contracts_are_available_without_a_value_or_codec() {
        struct Unserializable;

        const VEC: Shape = Vec::<Unserializable>::SHAPE;
        const ARRAY: Shape = <[Unserializable; 3]>::SHAPE;
        assert_eq!((VEC.family, VEC.version), ("vec", 1));
        assert_eq!(VEC.addressing, Addressing::Position);
        assert_eq!(VEC.length, Length::Variable);
        assert_eq!((ARRAY.family, ARRAY.version), ("array", 1));
        assert_eq!(ARRAY.addressing, Addressing::Position);
        assert_eq!(ARRAY.length, Length::Fixed(3));
        assert_eq!(<[Unserializable; 0]>::SHAPE.length, Length::Fixed(0));
        assert_eq!(VEC.order, IterationOrder::Position);
        assert_eq!(ARRAY.order, IterationOrder::Position);
        assert!(std::hint::black_box(VEC).allows_duplicate_values);
        assert!(std::hint::black_box(ARRAY).allows_duplicate_values);
    }

    #[test]
    fn id_records_keep_their_contract_under_restriction_and_updates() {
        let shape = IdRecords::SHAPE;
        assert_eq!((shape.family, shape.version), ("id_records", 1));
        assert_eq!(
            shape.addressing,
            Addressing::UniqueId {
                allocation: IdAllocation::Monotonic
            }
        );
        assert_eq!(shape.length, Length::Variable);
        assert_eq!(shape.order, IterationOrder::AscendingId);
        assert!(shape.allows_duplicate_values);

        let mut records = IdRecords::empty();
        for _ in 0..2 {
            records
                .apply(records.prepare_insert("repeated".into()).unwrap())
                .unwrap();
        }
        assert_eq!(
            records
                .restrict(..)
                .iter()
                .map(|record| record.id())
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(records.restrict(2..=2)[0].text(), "repeated");
        records.apply(IdChange::Delete { id: 1 }).unwrap();
        assert_eq!(records.restrict(..)[0].id(), 2);
        assert_eq!(records.next_id(), 3);
    }
}
