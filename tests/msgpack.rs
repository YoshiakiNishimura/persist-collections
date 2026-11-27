#![cfg(feature = "msgpack")]

use persist_collections::MessagePackSnapshot;
use std::{fs, io};

#[test]
fn messagepack_round_trips_vecs_and_large_arrays() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("vec.msgpack");
    let mut values = MessagePackSnapshot::new(&path)?.open_vec(vec![1_u32, 1000])?;
    values.get_mut().push(42);
    values.save()?;
    assert_eq!(
        MessagePackSnapshot::new(&path)?
            .open_vec(Vec::<u32>::new())?
            .into_inner(),
        vec![1, 1000, 42]
    );

    let path = dir.path().join("array.msgpack");
    MessagePackSnapshot::new(&path)?
        .open_array([42_u32; 64])?
        .save()?;
    assert_eq!(
        MessagePackSnapshot::new(&path)?
            .open_array([0_u32; 64])?
            .into_inner(),
        [42; 64]
    );
    assert!(
        MessagePackSnapshot::new(&path)?
            .open_array([0_u32; 1])
            .is_err()
    );

    let mut bytes = fs::read(&path)?;
    bytes.push(0);
    fs::write(&path, bytes)?;
    let error = MessagePackSnapshot::new(&path)?
        .open_vec(Vec::<u32>::new())
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    Ok(())
}
