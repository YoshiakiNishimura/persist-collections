use serde::{Deserialize, Serialize};
use std::{
    io,
    ops::{Bound, RangeBounds},
};

#[derive(Serialize, Deserialize)]
pub(super) enum Change<T> {
    Insert { id: u64, text: T, next_id: u64 },
    Delete { id: u64 },
}

pub(super) enum Applied {
    Insert { id: u64, position: usize },
    Delete { id: u64 },
}

impl Applied {
    pub(super) fn id(&self) -> u64 {
        match self {
            Self::Insert { id, .. } | Self::Delete { id } => *id,
        }
    }
}

#[derive(Deserialize, Serialize)]
pub(super) struct Record {
    pub(super) id: u64,
    pub(super) text: String,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Records {
    pub(super) next_id: u64,
    records: Vec<Record>,
}

impl Records {
    pub(super) fn empty() -> Self {
        Self {
            next_id: 1,
            records: Vec::new(),
        }
    }

    pub(super) fn validate(&self) -> io::Result<()> {
        let mut last_id = 0;
        for record in &self.records {
            if record.id <= last_id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "record IDs must be strictly increasing",
                ));
            }
            last_id = record.id;
        }
        if self.next_id <= last_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "next_id must exceed all record IDs",
            ));
        }
        Ok(())
    }

    /// Restrict the domain to an ID interval while preserving ID order.
    pub(super) fn restrict(&self, ids: impl RangeBounds<u64>) -> &[Record] {
        let start = match ids.start_bound() {
            Bound::Included(id) => self.records.partition_point(|record| record.id < *id),
            Bound::Excluded(id) => self.records.partition_point(|record| record.id <= *id),
            Bound::Unbounded => 0,
        };
        let rest = &self.records[start..];
        let len = match ids.end_bound() {
            Bound::Included(id) => rest.partition_point(|record| record.id <= *id),
            Bound::Excluded(id) => rest.partition_point(|record| record.id < *id),
            Bound::Unbounded => rest.len(),
        };
        &rest[..len]
    }

    pub(super) fn prepare_insert(&self, text: String) -> io::Result<Change<String>> {
        let id = self.next_id;
        let next_id = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("record IDs exhausted"))?;
        Ok(Change::Insert { id, text, next_id })
    }

    /// Apply an explicit change. Normal execution and WAL replay use this transition.
    pub(super) fn apply(&mut self, change: Change<String>) -> io::Result<Applied> {
        match change {
            Change::Insert { id, text, next_id } => {
                if id != self.next_id || id.checked_add(1) != Some(next_id) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid insert IDs",
                    ));
                }
                let position = self.records.len();
                self.records.push(Record { id, text });
                self.next_id = next_id;
                Ok(Applied::Insert { id, position })
            }
            Change::Delete { id } => {
                let position = self
                    .records
                    .binary_search_by_key(&id, |record| record.id)
                    .map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::NotFound,
                            format!("record {id} does not exist"),
                        )
                    })?;
                self.records.remove(position);
                Ok(Applied::Delete { id })
            }
        }
    }

    /// Borrow the change just applied to this state, before any further mutation.
    pub(super) fn applied_change(&self, applied: &Applied) -> Change<&str> {
        match *applied {
            Applied::Insert { id, position } => Change::Insert {
                id,
                text: &self.records[position].text,
                next_id: self.next_id,
            },
            Applied::Delete { id } => Change::Delete { id },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restrict_selects_id_intervals_without_changing_the_state() {
        let mut records = Records::empty();
        for text in ["first", "second", "third"] {
            records
                .apply(records.prepare_insert(text.into()).unwrap())
                .unwrap();
        }
        records.apply(Change::Delete { id: 2 }).unwrap();
        assert_eq!(records.restrict(1..=1)[0].text, "first");
        assert!(records.restrict(2..=2).is_empty());
        assert_eq!(
            records.restrict((Bound::Excluded(1), Bound::Included(3)))[0].id,
            3
        );
        assert!(
            records
                .restrict((Bound::Included(3), Bound::Excluded(1)))
                .is_empty()
        );
        assert_eq!(records.restrict(..).len(), 2);
        assert_eq!(records.next_id, 4);
    }

    #[test]
    fn invalid_changes_leave_the_state_intact() {
        let mut records = Records::empty();
        assert!(
            records
                .apply(Change::Insert {
                    id: 2,
                    text: "wrong ID".into(),
                    next_id: 3
                })
                .is_err()
        );
        assert!(records.apply(Change::Delete { id: 1 }).is_err());
        assert!(records.restrict(..).is_empty());
        assert_eq!(records.next_id, 1);
    }
}
