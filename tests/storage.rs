use persist_collections::{JsonSnapshot, Stored};
use serde::{Serialize, Serializer, ser::Error as _};
use std::{fs, io};

#[test]
fn memory_uses_standard_collections_without_serde() -> io::Result<()> {
    #[derive(Debug, PartialEq)]
    struct Local(u8);

    let mut values = Stored::memory(vec![Local(1)]);
    values.get_mut().push(Local(2));
    values.save()?;
    assert_eq!(values.into_inner(), [Local(1), Local(2)]);

    let mut array = Stored::memory([Local(3)]);
    array.get_mut()[0] = Local(4);
    array.save()?;
    assert_eq!(array.into_inner(), [Local(4)]);
    Ok(())
}

#[test]
fn vector_reopens_and_replaces_only_on_save() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("vec.json");
    let mut values = JsonSnapshot::new(&path)?.open_vec(vec![1, 2])?;
    assert!(!path.exists());
    values.save()?;

    values.get_mut().push(3);
    assert_eq!(
        JsonSnapshot::new(&path)?.open_vec(Vec::<i32>::new())?.get(),
        &[1, 2]
    );

    values.save()?;
    drop(values);
    assert_eq!(
        JsonSnapshot::new(&path)?.open_vec(Vec::<i32>::new())?.get(),
        &[1, 2, 3]
    );
    assert_eq!(fs::read_to_string(path)?, "[1,2,3]");
    Ok(())
}

#[test]
fn arrays_support_zero_and_more_than_32_elements() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("array.json");
    let mut values = JsonSnapshot::new(&path)?.open_array([0; 64])?;
    values.get_mut()[63] = 42;
    values.save()?;
    let reopened = JsonSnapshot::new(&path)?.open_array([1; 64])?;
    assert_eq!(reopened.get(), values.get());

    let empty: [i32; 0] = [];
    let path = dir.path().join("empty.json");
    JsonSnapshot::new(&path)?.open_array(empty)?.save()?;
    assert_eq!(
        JsonSnapshot::new(&path)?.open_array(empty)?.into_inner(),
        empty
    );
    Ok(())
}

#[test]
fn invalid_snapshots_surface_errors_without_modifying_the_file() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("invalid.json");
    for contents in ["", "[", "null", "[false]", "[1] true"] {
        fs::write(&path, contents)?;
        assert!(JsonSnapshot::new(&path)?.open_vec(vec![0]).is_err());
        assert_eq!(fs::read_to_string(&path)?, contents);
    }

    fs::write(&path, "[1,2]")?;
    let error = JsonSnapshot::new(&path)?.open_array([0]).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(
        error
            .to_string()
            .contains("expected 1 array elements, found 2")
    );
    assert!(JsonSnapshot::new(dir.path())?.open_vec(vec![0]).is_err());
    Ok(())
}

#[test]
fn failed_serialization_keeps_previous_snapshot() -> io::Result<()> {
    struct Fails;

    impl Serialize for Fails {
        fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(S::Error::custom("intentional serialization failure"))
        }
    }

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("previous.json");
    JsonSnapshot::new(&path)?.open_vec(vec![1])?.save()?;
    let values = Stored::with_backend(vec![Fails], JsonSnapshot::new(&path)?);
    assert!(values.save().is_err());
    assert_eq!(fs::read_to_string(path)?, "[1]");
    assert_eq!(fs::read_dir(dir.path())?.count(), 1);
    Ok(())
}
