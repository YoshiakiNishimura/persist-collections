use persist_collections::{Checked, FileSnapshot, Json, SnapshotFormat};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    marker::PhantomData,
    path::Path,
};

const VERSION: u8 = 1;
const FANOUT: usize = 16;
const MAX_PAGE_BYTES: usize = 16 * 1024 * 1024;
const PAGES: &str = "pages";

#[derive(Clone, Copy, Deserialize, Serialize)]
struct Link {
    first: u64,
    offset: u64,
}

#[derive(Deserialize, Serialize)]
struct Root {
    version: u8,
    codec: String,
    next_id: u64,
    end: u64,
    tree: Option<Link>,
}

#[derive(Deserialize, Serialize)]
enum Page {
    Leaf(Vec<(u64, String)>),
    Branch(Vec<Link>),
}

pub(super) struct Tree<'a, F> {
    directory: &'a Path,
    root: Root,
    initialized: bool,
    format: PhantomData<fn() -> F>,
}

impl<'a, F: SnapshotFormat> Tree<'a, F> {
    // The caller holds the directory's store lock throughout this view.
    pub(super) fn open(directory: &'a Path, codec: &str) -> io::Result<Self> {
        let manifest = FileSnapshot::<Checked<Json>>::new(directory.join("root"))?;
        let (root, initialized) = match manifest.read_value::<Root>() {
            Ok(root) => (root, true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if directory.join(PAGES).try_exists()? || directory.join("format").try_exists()? {
                    return Err(invalid(
                        "missing B+ tree root or directory belongs to a WAL store",
                    ));
                }
                (
                    Root {
                        version: VERSION,
                        codec: codec.into(),
                        next_id: 1,
                        end: 0,
                        tree: None,
                    },
                    false,
                )
            }
            Err(error) => return Err(error),
        };
        if root.version != VERSION || root.codec != codec {
            return Err(invalid("B+ tree uses a different format or version"));
        }
        if root.next_id == 0
            || root.tree.is_some_and(|link| {
                link.first == 0 || link.first >= root.next_id || link.offset >= root.end
            })
        {
            return Err(invalid("invalid B+ tree root"));
        }
        Ok(Self {
            directory,
            root,
            initialized,
            format: PhantomData,
        })
    }

    pub(super) fn get(&self, key: u64) -> io::Result<Option<String>> {
        let Some(mut link) = self.root.tree else {
            return Ok(None);
        };
        if key < link.first {
            return Ok(None);
        }
        let mut pages = self.reader()?;
        loop {
            match pages.read(link)? {
                Page::Leaf(rows) => {
                    return Ok(rows
                        .into_iter()
                        .find_map(|(id, text)| (id == key).then_some(text)));
                }
                Page::Branch(children) => link = children[child_index(&children, key)],
            }
        }
    }

    pub(super) fn visit(
        &self,
        first: u64,
        last: u64,
        mut emit: impl FnMut(u64, &str) -> io::Result<()>,
    ) -> io::Result<()> {
        if let Some(root) = self.root.tree
            && first <= last
        {
            self.reader()?.visit(root, first, last, &mut emit)?;
        }
        Ok(())
    }

    pub(super) fn insert(mut self, text: String) -> io::Result<u64> {
        let id = self.root.next_id;
        let next_id = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("record IDs exhausted"))?;
        let manifest = FileSnapshot::<Checked<Json>>::new(self.directory.join("root"))?;
        if !self.initialized {
            manifest.create_value(&self.root)?;
        }
        let mut pages = self.writer()?;
        let tree = match self.root.tree {
            None => pages.append(Page::Leaf(vec![(id, text)]))?,
            Some(root) => {
                let (left, right) = pages.insert(root, id, text)?;
                if let Some(right) = right {
                    pages.append(Page::Branch(vec![left, right]))?
                } else {
                    left
                }
            }
        };
        self.root.next_id = next_id;
        self.publish(pages, Some(tree))?;
        Ok(id)
    }

    /// A missing key leaves the root and page file untouched.
    pub(super) fn delete(self, id: u64) -> io::Result<bool> {
        let Some(root) = self.root.tree else {
            return Ok(false);
        };
        let mut pages = self.writer()?;
        match pages.delete(root, id, true)? {
            Deletion::Missing => Ok(false),
            Deletion::Removed(link) => self.publish(pages, link).map(|()| true),
        }
    }

    fn reader(&self) -> io::Result<Pages<F>> {
        self.pages(File::open(self.directory.join(PAGES))?)
    }

    fn writer(&self) -> io::Result<Pages<F>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(self.root.end == 0)
            .truncate(false)
            .open(self.directory.join(PAGES))?;
        self.pages(file)
    }

    fn pages(&self, file: File) -> io::Result<Pages<F>> {
        let tail = file.metadata()?.len();
        if tail < self.root.end {
            return Err(invalid(
                "B+ tree page file is shorter than the published root",
            ));
        }
        Ok(Pages {
            file,
            end: self.root.end,
            tail,
            format: PhantomData,
        })
    }

    fn publish(mut self, pages: Pages<F>, tree: Option<Link>) -> io::Result<()> {
        pages.file.sync_all()?;
        self.root.tree = tree;
        self.root.end = pages.tail;
        // The new root becomes visible only after all new pages have been synced.
        FileSnapshot::<Checked<Json>>::new(self.directory.join("root"))?.save_value(&self.root)
    }
}

struct Pages<F> {
    file: File,
    end: u64,
    tail: u64,
    format: PhantomData<fn() -> F>,
}

enum Deletion {
    Missing,
    Removed(Option<Link>),
}

impl<F: SnapshotFormat> Pages<F> {
    fn read(&mut self, link: Link) -> io::Result<Page> {
        let payload_start = link
            .offset
            .checked_add(4)
            .filter(|end| *end <= self.end)
            .ok_or_else(|| invalid("B+ tree page header is outside the published file"))?;
        self.file.seek(SeekFrom::Start(link.offset))?;
        let mut header = [0; 4];
        self.file.read_exact(&mut header)?;
        let len = u32::from_le_bytes(header) as usize;
        if len == 0
            || len > MAX_PAGE_BYTES
            || payload_start
                .checked_add(len as u64)
                .is_none_or(|end| end > self.end)
        {
            return Err(invalid("invalid B+ tree page length"));
        }
        let mut payload = vec![0; len];
        self.file.read_exact(&mut payload)?;
        let page = Checked::<F>::decode(payload.as_slice())?;
        let valid = match &page {
            Page::Leaf(rows) => {
                rows.len() <= FANOUT
                    && rows.first().is_some_and(|row| row.0 == link.first)
                    && rows.windows(2).all(|pair| pair[0].0 < pair[1].0)
            }
            Page::Branch(children) => {
                children.len() <= FANOUT
                    && children
                        .first()
                        .is_some_and(|child| child.first == link.first)
                    && children
                        .windows(2)
                        .all(|pair| pair[0].first < pair[1].first)
                    && children.iter().all(|child| child.offset < link.offset)
            }
        };
        if !valid {
            return Err(invalid("invalid B+ tree page ordering or child link"));
        }
        Ok(page)
    }

    fn append(&mut self, page: Page) -> io::Result<Link> {
        let first = match &page {
            Page::Leaf(rows) => rows[0].0,
            Page::Branch(children) => children[0].first,
        };
        let mut payload = Vec::new();
        Checked::<F>::encode(&mut payload, &page)?;
        if payload.len() > MAX_PAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "B+ tree page exceeds 16 MiB",
            ));
        }
        let offset = self.tail;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(&(payload.len() as u32).to_le_bytes())?;
        self.file.write_all(&payload)?;
        self.tail += 4 + payload.len() as u64;
        Ok(Link { first, offset })
    }

    fn insert(&mut self, link: Link, key: u64, value: String) -> io::Result<(Link, Option<Link>)> {
        let (left, right) = match self.read(link)? {
            Page::Leaf(mut rows) => {
                let position = rows
                    .binary_search_by_key(&key, |row| row.0)
                    .err()
                    .ok_or_else(|| invalid("duplicate allocated B+ tree ID"))?;
                rows.insert(position, (key, value));
                let right =
                    (rows.len() > FANOUT).then(|| Page::Leaf(rows.split_off(rows.len() / 2)));
                (Page::Leaf(rows), right)
            }
            Page::Branch(mut children) => {
                let slot = child_index(&children, key);
                let (left, right) = self.insert(children[slot], key, value)?;
                children[slot] = left;
                if let Some(right) = right {
                    children.insert(slot + 1, right);
                }
                let right = (children.len() > FANOUT)
                    .then(|| Page::Branch(children.split_off(children.len() / 2)));
                (Page::Branch(children), right)
            }
        };
        Ok((
            self.append(left)?,
            right.map(|page| self.append(page)).transpose()?,
        ))
    }

    fn delete(&mut self, link: Link, key: u64, root: bool) -> io::Result<Deletion> {
        let page = match self.read(link)? {
            Page::Leaf(mut rows) => {
                let Ok(position) = rows.binary_search_by_key(&key, |row| row.0) else {
                    return Ok(Deletion::Missing);
                };
                rows.remove(position);
                (!rows.is_empty()).then_some(Page::Leaf(rows))
            }
            Page::Branch(mut children) => {
                let slot = child_index(&children, key);
                match self.delete(children[slot], key, false)? {
                    Deletion::Missing => return Ok(Deletion::Missing),
                    Deletion::Removed(Some(child)) => children[slot] = child,
                    Deletion::Removed(None) => {
                        children.remove(slot);
                    }
                }
                if root && children.len() == 1 {
                    return Ok(Deletion::Removed(Some(children[0])));
                }
                (!children.is_empty()).then_some(Page::Branch(children))
            }
        };
        Ok(Deletion::Removed(
            page.map(|page| self.append(page)).transpose()?,
        ))
    }

    fn visit(
        &mut self,
        link: Link,
        first: u64,
        last: u64,
        emit: &mut impl FnMut(u64, &str) -> io::Result<()>,
    ) -> io::Result<()> {
        match self.read(link)? {
            Page::Leaf(rows) => {
                let start = rows.partition_point(|row| row.0 < first);
                for (id, text) in rows[start..].iter().take_while(|row| row.0 <= last) {
                    emit(*id, text)?;
                }
            }
            Page::Branch(children) => {
                for &child in &children[child_index(&children, first)..] {
                    if child.first > last {
                        break;
                    }
                    self.visit(child, first, last, emit)?;
                }
            }
        }
        Ok(())
    }
}

fn child_index(children: &[Link], key: u64) -> usize {
    children
        .partition_point(|child| child.first <= key)
        .saturating_sub(1)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn inserts_split_pages_and_previous_root_still_reads_previous_state() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path();
        for id in 1..=160 {
            assert_eq!(
                Tree::<Json>::open(path, "json")?.insert(format!("note {id}"))?,
                id
            );
        }
        let old_root = fs::read(path.join("root"))?;
        let old_pages = fs::read(path.join(PAGES))?;
        let mut range = Vec::new();
        Tree::<Json>::open(path, "json")?.visit(9, 26, |id, _| {
            range.push(id);
            Ok(())
        })?;
        assert_eq!(range, (9..=26).collect::<Vec<_>>());

        assert!(Tree::<Json>::open(path, "json")?.delete(9)?);
        assert_eq!(Tree::<Json>::open(path, "json")?.insert("new".into())?, 161);
        assert!(fs::read(path.join(PAGES))?.starts_with(&old_pages));
        let current_root = fs::read(path.join("root"))?;
        fs::write(path.join("root"), old_root)?;
        let old_tree = Tree::<Json>::open(path, "json")?;
        assert_eq!(old_tree.get(9)?.as_deref(), Some("note 9"));
        assert_eq!(old_tree.get(161)?, None);
        fs::write(path.join("root"), current_root)?;
        let tree = Tree::<Json>::open(path, "json")?;
        assert_eq!(tree.get(9)?, None);
        assert_eq!(tree.get(161)?.as_deref(), Some("new"));
        Ok(())
    }

    #[test]
    fn point_and_range_reads_skip_unrelated_pages() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path();
        for id in 1..=40 {
            Tree::<Json>::open(path, "json")?.insert(format!("note {id}"))?;
        }
        let tree = Tree::<Json>::open(path, "json")?;
        let Page::Branch(children) = tree.reader()?.read(tree.root.tree.unwrap())? else {
            panic!("the root should have split into branches");
        };
        let mut file = OpenOptions::new().write(true).open(path.join(PAGES))?;
        // Damage the leftmost live child. Queries addressing only the right side still work.
        file.seek(SeekFrom::Start(children[0].offset + 4))?;
        file.write_all(b"x")?;
        drop(file);
        assert_eq!(tree.get(40)?.as_deref(), Some("note 40"));
        assert!(tree.get(1).is_err());
        let mut rows = Vec::new();
        tree.visit(38, 40, |id, _| {
            rows.push(id);
            Ok(())
        })?;
        assert_eq!(rows, [38, 39, 40]);
        Ok(())
    }
}
