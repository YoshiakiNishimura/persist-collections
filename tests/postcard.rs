#![cfg(feature = "postcard")]

use persist_collections::PostcardSnapshot;
use std::{fs, io};

#[test]
fn postcard_round_trips_and_checks_its_file_envelope() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("snapshot.postcard");
    PostcardSnapshot::new(&path)?
        .open_array([300_u32; 64])?
        .save()?;
    let reopened = PostcardSnapshot::new(&path)?.open_array([0_u32; 64])?;
    assert_eq!(reopened.into_inner(), [300; 64]);
    assert_eq!(
        PostcardSnapshot::new(&path)?
            .open_vec(Vec::<u32>::new())?
            .into_inner(),
        vec![300; 64]
    );

    let snapshot = fs::read(&path)?;
    assert_eq!(&snapshot[..5], b"PCSN\x01");
    let mut wrong_version = snapshot.clone();
    wrong_version[4] = 2;
    let mut trailing = snapshot.clone();
    trailing.push(0);
    for corrupt in [
        wrong_version,
        trailing,
        snapshot[..snapshot.len() - 1].to_vec(),
    ] {
        fs::write(&path, corrupt)?;
        let error = PostcardSnapshot::new(&path)?
            .open_vec(Vec::<u32>::new())
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
    Ok(())
}
