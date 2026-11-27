use serde::{Deserialize, Serialize};
use std::io;

const FANOUT: usize = 16;

#[derive(Clone, Copy, Deserialize, Serialize)]
pub(super) struct Link<K, A> {
    pub(super) first: K,
    #[serde(rename = "offset")]
    pub(super) address: A,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) enum Page<K, V, A> {
    Leaf(Vec<(K, V)>),
    Branch(Vec<Link<K, A>>),
}

pub(super) trait ReadPages<K, V> {
    type Address: Copy;

    fn read_page(&mut self, address: Self::Address) -> io::Result<Page<K, V, Self::Address>>;
}

pub(super) trait WritePages<K, V>: ReadPages<K, V> {
    /// Write a new or replacement page. The strategy decides whether to reuse
    /// the previous address, and is responsible for staging and publication.
    fn write_page(
        &mut self,
        previous: Option<Self::Address>,
        page: Page<K, V, Self::Address>,
    ) -> io::Result<Self::Address>;
}

pub(super) struct BPlusTree<K, A> {
    root: Option<Link<K, A>>,
}

enum Deletion<K, A> {
    Missing,
    Removed(Option<Link<K, A>>),
}

type Split<K, A> = (Link<K, A>, Option<Link<K, A>>);

impl<K: Copy + Ord, A: Copy> BPlusTree<K, A> {
    pub(super) fn from_root(root: Option<Link<K, A>>) -> Self {
        Self { root }
    }

    pub(super) fn root(&self) -> Option<Link<K, A>> {
        self.root
    }

    pub(super) fn get<V>(
        &self,
        pages: &mut impl ReadPages<K, V, Address = A>,
        key: K,
    ) -> io::Result<Option<V>> {
        let Some(mut link) = self.root else {
            return Ok(None);
        };
        if key < link.first {
            return Ok(None);
        }
        loop {
            match read(pages, link)? {
                Page::Leaf(mut rows) => {
                    return Ok(rows
                        .binary_search_by_key(&key, |row| row.0)
                        .ok()
                        .map(|position| rows.swap_remove(position).1));
                }
                Page::Branch(children) => link = children[child_index(&children, key)],
            }
        }
    }

    pub(super) fn visit<V>(
        &self,
        pages: &mut impl ReadPages<K, V, Address = A>,
        first: K,
        last: K,
        mut emit: impl FnMut(K, &V) -> io::Result<()>,
    ) -> io::Result<()> {
        if let Some(root) = self.root
            && first <= last
        {
            visit(pages, root, first, last, &mut emit)?;
        }
        Ok(())
    }

    pub(super) fn insert<V>(
        &mut self,
        pages: &mut impl WritePages<K, V, Address = A>,
        key: K,
        value: V,
    ) -> io::Result<()> {
        let root = match self.root {
            None => write(pages, None, Page::Leaf(vec![(key, value)]))?,
            Some(root) => {
                let (left, right) = insert(pages, root, key, value)?;
                match right {
                    Some(right) => write(pages, None, Page::Branch(vec![left, right]))?,
                    None => left,
                }
            }
        };
        self.root = Some(root);
        Ok(())
    }

    pub(super) fn delete<V>(
        &mut self,
        pages: &mut impl WritePages<K, V, Address = A>,
        key: K,
    ) -> io::Result<bool> {
        let Some(root) = self.root else {
            return Ok(false);
        };
        match delete(pages, root, key, true)? {
            Deletion::Missing => Ok(false),
            Deletion::Removed(root) => {
                self.root = root;
                Ok(true)
            }
        }
    }
}

fn read<K: Copy + Ord, V, A: Copy>(
    pages: &mut impl ReadPages<K, V, Address = A>,
    link: Link<K, A>,
) -> io::Result<Page<K, V, A>> {
    let page = pages.read_page(link.address)?;
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
        }
    };
    if !valid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid B+ tree page ordering",
        ));
    }
    Ok(page)
}

fn write<K: Copy, V, A: Copy>(
    pages: &mut impl WritePages<K, V, Address = A>,
    previous: Option<A>,
    page: Page<K, V, A>,
) -> io::Result<Link<K, A>> {
    let first = match &page {
        Page::Leaf(rows) => rows[0].0,
        Page::Branch(children) => children[0].first,
    };
    Ok(Link {
        first,
        address: pages.write_page(previous, page)?,
    })
}

fn insert<K: Copy + Ord, V, A: Copy>(
    pages: &mut impl WritePages<K, V, Address = A>,
    link: Link<K, A>,
    key: K,
    value: V,
) -> io::Result<Split<K, A>> {
    let (left, right) = match read(pages, link)? {
        Page::Leaf(mut rows) => {
            let position = rows
                .binary_search_by_key(&key, |row| row.0)
                .err()
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "duplicate B+ tree key")
                })?;
            rows.insert(position, (key, value));
            let right = (rows.len() > FANOUT).then(|| Page::Leaf(rows.split_off(rows.len() / 2)));
            (Page::Leaf(rows), right)
        }
        Page::Branch(mut children) => {
            let slot = child_index(&children, key);
            let (left, right) = insert(pages, children[slot], key, value)?;
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
        write(pages, Some(link.address), left)?,
        right.map(|page| write(pages, None, page)).transpose()?,
    ))
}

fn delete<K: Copy + Ord, V, A: Copy>(
    pages: &mut impl WritePages<K, V, Address = A>,
    link: Link<K, A>,
    key: K,
    root: bool,
) -> io::Result<Deletion<K, A>> {
    let page = match read(pages, link)? {
        Page::Leaf(mut rows) => {
            let Ok(position) = rows.binary_search_by_key(&key, |row| row.0) else {
                return Ok(Deletion::Missing);
            };
            rows.remove(position);
            (!rows.is_empty()).then_some(Page::Leaf(rows))
        }
        Page::Branch(mut children) => {
            let slot = child_index(&children, key);
            match delete(pages, children[slot], key, false)? {
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
        page.map(|page| write(pages, Some(link.address), page))
            .transpose()?,
    ))
}

fn visit<K: Copy + Ord, V, A: Copy>(
    pages: &mut impl ReadPages<K, V, Address = A>,
    link: Link<K, A>,
    first: K,
    last: K,
    emit: &mut impl FnMut(K, &V) -> io::Result<()>,
) -> io::Result<()> {
    match read(pages, link)? {
        Page::Leaf(rows) => {
            let start = rows.partition_point(|row| row.0 < first);
            for (key, value) in rows[start..].iter().take_while(|row| row.0 <= last) {
                emit(*key, value)?;
            }
        }
        Page::Branch(children) => {
            for &child in &children[child_index(&children, first)..] {
                if child.first > last {
                    break;
                }
                visit(pages, child, first, last, emit)?;
            }
        }
    }
    Ok(())
}

fn child_index<K: Ord, A>(children: &[Link<K, A>], key: K) -> usize {
    children
        .partition_point(|child| child.first <= key)
        .saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct InPlace {
        pages: Vec<Page<i32, i32, usize>>,
        rewrites: usize,
    }

    impl ReadPages<i32, i32> for InPlace {
        type Address = usize;

        fn read_page(&mut self, at: usize) -> io::Result<Page<i32, i32, usize>> {
            Ok(self.pages[at].clone())
        }
    }

    impl WritePages<i32, i32> for InPlace {
        fn write_page(
            &mut self,
            previous: Option<usize>,
            page: Page<i32, i32, usize>,
        ) -> io::Result<usize> {
            if let Some(at) = previous {
                self.pages[at] = page;
                self.rewrites += 1;
                Ok(at)
            } else {
                let at = self.pages.len();
                self.pages.push(page);
                Ok(at)
            }
        }
    }

    #[test]
    fn tree_supports_in_place_pages_without_a_file_or_publication_strategy() -> io::Result<()> {
        let mut pages = InPlace::default();
        let mut tree = BPlusTree::from_root(None);
        for step in 0..257 {
            let id = step * 73 % 257;
            tree.insert(&mut pages, id, id * 2)?;
        }
        assert!(pages.rewrites > 0);
        for key in (10..=20).step_by(2) {
            assert!(tree.delete(&mut pages, key)?);
        }
        assert!(!tree.delete(&mut pages, 10)?);
        assert_eq!(tree.get(&mut pages, 17)?, Some(34));
        assert!(tree.insert(&mut pages, 17, 99).is_err());
        let mut rows = Vec::new();
        tree.visit(&mut pages, 10, 20, |key, value| {
            rows.push((key, *value));
            Ok(())
        })?;
        assert_eq!(rows, [(11, 22), (13, 26), (15, 30), (17, 34), (19, 38)]);
        Ok(())
    }
}
