use crate::{IdRecord, IdRecords, Selection};
use std::ops::RangeInclusive;

/// An inspectable function from a record to a truth value.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum RecordPredicate<T> {
    IdBetween(RangeInclusive<u64>),
    TextEq(T),
    And(Box<Self>, Box<Self>),
}

impl<T> RecordPredicate<T> {
    /// Compose restrictions by intersection.
    pub fn and(self, other: Self) -> Self {
        Self::And(Box::new(self), Box::new(other))
    }

    /// Return an ID interval containing every possible match.
    ///
    /// An empty interval has its start greater than its end.
    pub fn id_bounds(&self) -> RangeInclusive<u64> {
        match self {
            Self::IdBetween(bounds) => bounds.clone(),
            Self::TextEq(_) => 1..=u64::MAX,
            Self::And(left, right) => {
                let left = left.id_bounds();
                let right = right.id_bounds();
                (*left.start()).max(*right.start())..=(*left.end()).min(*right.end())
            }
        }
    }
}

impl<T: AsRef<str>> RecordPredicate<T> {
    /// Evaluate the full predicate against a candidate, including any residual filter.
    pub fn matches(&self, id: u64, text: &str) -> bool {
        match self {
            Self::IdBetween(bounds) => bounds.contains(&id),
            Self::TextEq(expected) => text == expected.as_ref(),
            Self::And(left, right) => left.matches(id, text) && right.matches(id, text),
        }
    }
}

impl<T: AsRef<str>> Selection<IdRecords> for RecordPredicate<T> {
    fn select(self, collection: &IdRecords) -> impl Iterator<Item = (u64, &IdRecord)> {
        collection
            .restrict(self.id_bounds())
            .iter()
            .filter(move |record| self.matches(record.id(), record.text()))
            .map(|record| (record.id(), record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{All, restrict};

    #[test]
    fn composed_restriction_agrees_with_sequential_filters() -> std::io::Result<()> {
        let mut records = IdRecords::empty();
        for text in ["note", "other", "note", "note", "other"] {
            records.apply(records.prepare_insert(text.into())?)?;
        }
        let left = RecordPredicate::IdBetween(2..=5);
        let right = RecordPredicate::TextEq("note");
        let combined = left.clone().and(right.clone());
        assert_eq!(combined.id_bounds(), 2..=5);

        let sequential: Vec<_> = restrict(&records, All)
            .filter(|(id, record)| left.matches(*id, record.text()))
            .filter(|(id, record)| right.matches(*id, record.text()))
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            restrict(&records, combined)
                .map(|(id, _)| id)
                .collect::<Vec<_>>(),
            sequential
        );
        assert_eq!(sequential, [3, 4]);

        let disjoint = left.and(RecordPredicate::IdBetween(8..=10));
        assert!(disjoint.id_bounds().is_empty());
        assert!(restrict(&records, disjoint).next().is_none());
        Ok(())
    }
}
