use crate::{InsertAllocated, InsertAllocatedKey, RemoveByKey, RemoveKey, apply};
use serde::{Deserialize, Serialize};
use std::{
    io,
    ops::{Bound, RangeBounds},
};

/// An explicit change to an ID-keyed collection.
#[derive(Serialize, Deserialize)]
#[serde(rename = "Change")]
pub enum IdChange<T> {
    Insert { id: u64, text: T, next_id: u64 },
    Delete { id: u64 },
}

/// A text record with a stable ID within its collection.
#[derive(Deserialize, Serialize)]
#[serde(rename = "Record")]
pub struct IdRecord {
    id: u64,
    text: String,
}

impl IdRecord {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

/// An ID-ordered collection of text records and its next allocation state.
#[derive(Deserialize, Serialize)]
#[serde(rename = "Records", try_from = "UncheckedRecords")]
pub struct IdRecords {
    next_id: u64,
    records: Vec<IdRecord>,
}

#[derive(Deserialize)]
#[serde(rename = "Records")]
struct UncheckedRecords {
    next_id: u64,
    records: Vec<IdRecord>,
}

impl TryFrom<UncheckedRecords> for IdRecords {
    type Error = io::Error;

    fn try_from(raw: UncheckedRecords) -> io::Result<Self> {
        let records = Self {
            next_id: raw.next_id,
            records: raw.records,
        };
        records.validate()?;
        Ok(records)
    }
}

impl IdRecords {
    pub fn empty() -> Self {
        Self {
            next_id: 1,
            records: Vec::new(),
        }
    }

    fn validate(&self) -> io::Result<()> {
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

    /// The allocation counter stored with this collection.
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    fn position(&self, id: u64) -> Option<usize> {
        self.records
            .binary_search_by_key(&id, |record| record.id)
            .ok()
    }

    pub(crate) fn find(&self, id: u64) -> Option<&IdRecord> {
        self.position(id).map(|position| &self.records[position])
    }

    /// Restrict the domain to an ID interval while preserving ID order.
    pub fn restrict(&self, ids: impl RangeBounds<u64>) -> &[IdRecord] {
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

    /// Prepare an insert by reading the allocation counter.
    pub fn prepare_insert(&self, text: String) -> io::Result<IdChange<String>> {
        let id = self.next_id;
        let next_id = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("record IDs exhausted"))?;
        Ok(IdChange::Insert { id, text, next_id })
    }

    /// Apply an explicit change. Normal execution and WAL replay use this transition.
    pub fn apply(&mut self, change: IdChange<String>) -> io::Result<()> {
        match change {
            IdChange::Insert { id, text, next_id } => apply(
                self,
                InsertAllocated {
                    key: id,
                    value: text,
                    next_key: next_id,
                },
            ),
            IdChange::Delete { id } => apply(self, RemoveByKey(id)),
        }
    }
}

impl InsertAllocatedKey for IdRecords {
    type Value = String;

    fn insert_allocated_key(&mut self, id: u64, text: String, next_id: u64) -> io::Result<()> {
        if id != self.next_id || id.checked_add(1) != Some(next_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid insert IDs",
            ));
        }
        self.records.push(IdRecord { id, text });
        self.next_id = next_id;
        Ok(())
    }
}

impl RemoveKey for IdRecords {
    fn remove_key(&mut self, id: u64) -> io::Result<()> {
        let position = self.position(id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("record {id} does not exist"),
            )
        })?;
        self.records.remove(position);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restrict_selects_id_intervals_without_changing_the_state() {
        let mut records = IdRecords::empty();
        for text in ["first", "second", "third"] {
            records
                .apply(records.prepare_insert(text.into()).unwrap())
                .unwrap();
        }
        records.apply(IdChange::Delete { id: 2 }).unwrap();
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
        let mut records = IdRecords::empty();
        assert!(
            records
                .apply(IdChange::Insert {
                    id: 2,
                    text: "wrong ID".into(),
                    next_id: 3
                })
                .is_err()
        );
        assert!(records.apply(IdChange::Delete { id: 1 }).is_err());
        assert!(records.restrict(..).is_empty());
        assert_eq!(records.next_id, 1);
    }

    #[cfg(feature = "json")]
    #[test]
    fn deserialization_validates_the_id_contract() {
        let valid = r#"{"next_id":3,"records":[{"id":2,"text":"kept"}]}"#;
        let decoded: IdRecords = serde_json::from_str(valid).unwrap();
        assert_eq!(serde_json::to_string(&decoded).unwrap(), valid);
        let invalid = r#"{"next_id":3,"records":[{"id":2,"text":"a"},{"id":2,"text":"b"}]}"#;
        assert!(serde_json::from_str::<IdRecords>(invalid).is_err());
    }
}
