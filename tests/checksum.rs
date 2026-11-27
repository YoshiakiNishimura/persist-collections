#![cfg(all(feature = "checksum", feature = "json"))]

use persist_collections::{Checked, FileSnapshot, Json};
use std::{fs, io};

#[test]
fn checksum_detects_a_change_to_valid_json() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("checked");
    let snapshot = FileSnapshot::<Checked<Json>>::new(&path)?;
    snapshot.create_value(&[123])?;
    assert_eq!(snapshot.read_value::<Vec<i32>>()?, [123]);
    assert_eq!(
        snapshot.create_value(&[456]).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(snapshot.read_value::<Vec<i32>>()?, [123]);

    let mut bytes = fs::read(&path)?;
    let position = bytes.iter().position(|byte| *byte == b'2').unwrap();
    bytes[position] = b'9';
    assert_eq!(
        serde_json::from_slice::<Vec<i32>>(&bytes[5..bytes.len() - 4])?,
        [193]
    );
    fs::write(&path, bytes)?;
    let error = snapshot.read_value::<Vec<i32>>().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(error.to_string(), "snapshot checksum mismatch");
    Ok(())
}
