//! An in-memory virtual filesystem (CLAUDE.md §39 the virtual filesystem; §44, §49 the
//! runtime and SDK need a file namespace). A tree of directories and files held in RAM,
//! with the path handling and the node operations the kernel and services build on. Pure
//! (no I/O, no `unsafe`), tested on the host; an on-disk filesystem (which needs its own
//! format ADR) will plug in under the same operations later.
//!
//! Paths are absolute, `/`-separated, UTF-8. `.` is the current directory and `..` the
//! parent (the root's parent is itself). A component is at most [`MAX_NAME`] bytes; a
//! trailing slash is allowed. There are no symbolic links yet.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

/// The longest a single path component may be.
pub const MAX_NAME: usize = 255;

/// Why an operation was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsError {
    /// A path component, or the whole path, does not name an existing entry.
    NotFound,
    /// A path component that must be a directory is a file.
    NotDirectory,
    /// The target is a directory where a file was required (or vice versa).
    IsDirectory,
    /// Something already exists at the path.
    Exists,
    /// A directory to remove is not empty.
    NotEmpty,
    /// The path is malformed (empty, not absolute, or a component over [`MAX_NAME`]).
    BadPath,
    /// The node table is full, or a file would exceed the size limit.
    NoSpace,
    /// A handle or node id does not name a live node.
    BadHandle,
}

/// What a directory entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Directory,
    File,
}

/// A node's metadata (what `stat` returns).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub kind: Kind,
    /// File length in bytes, or the number of entries for a directory.
    pub size: usize,
    /// The node's id (stable while it lives).
    pub id: usize,
}

/// One directory entry: a name and the node it names.
#[derive(Clone, Debug)]
struct Entry {
    name: String,
    node: usize,
}

#[derive(Debug)]
enum Body {
    Dir(Vec<Entry>),
    File(Vec<u8>),
}

#[derive(Debug)]
struct Node {
    body: Body,
    parent: usize,
}

/// A tree of directories and files in memory.
#[derive(Debug)]
pub struct Vfs {
    nodes: Vec<Option<Node>>,
    free: Vec<usize>,
    /// Largest a single file may grow, and the most nodes the tree may hold.
    max_file: usize,
    max_nodes: usize,
}

/// One parsed, non-empty path component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Component<'a> {
    /// `.`
    Current,
    /// `..`
    Parent,
    /// A name.
    Name(&'a str),
}

/// Splits an absolute path into its components, or `None` if it is empty, not absolute, or
/// has a component longer than [`MAX_NAME`]. Empty components (from `//` or a trailing
/// slash) are skipped.
pub fn components(path: &str) -> Option<impl Iterator<Item = Component<'_>>> {
    if path.is_empty() || !path.starts_with('/') {
        return None;
    }
    if path.split('/').any(|c| c.len() > MAX_NAME) {
        return None;
    }
    Some(path.split('/').filter(|c| !c.is_empty()).map(|c| match c {
        "." => Component::Current,
        ".." => Component::Parent,
        name => Component::Name(name),
    }))
}

impl Vfs {
    /// An empty filesystem: just the root directory. `max_file` bounds a single file's
    /// size and `max_nodes` the whole tree.
    pub fn new(max_file: usize, max_nodes: usize) -> Self {
        let root = Node {
            body: Body::Dir(Vec::new()),
            parent: 0,
        };
        Self {
            nodes: alloc::vec![Some(root)],
            free: Vec::new(),
            max_file,
            max_nodes: max_nodes.max(1),
        }
    }

    const ROOT: usize = 0;

    fn get(&self, id: usize) -> Result<&Node, FsError> {
        self.nodes
            .get(id)
            .and_then(Option::as_ref)
            .ok_or(FsError::BadHandle)
    }

    fn live_nodes(&self) -> usize {
        self.nodes.iter().filter(|n| n.is_some()).count()
    }

    /// Resolves `path` to a node id, following `.` and `..`.
    pub fn resolve(&self, path: &str) -> Result<usize, FsError> {
        let comps = components(path).ok_or(FsError::BadPath)?;
        let mut here = Self::ROOT;
        for comp in comps {
            here = match comp {
                Component::Current => self.directory(here)?,
                Component::Parent => self.parent_of(here)?,
                Component::Name(name) => self.child(here, name)?,
            };
        }
        Ok(here)
    }

    /// `id`, which must be a directory: `.` and `..` mean nothing after a file.
    fn directory(&self, id: usize) -> Result<usize, FsError> {
        match self.get(id)?.body {
            Body::Dir(_) => Ok(id),
            Body::File(_) => Err(FsError::NotDirectory),
        }
    }

    /// The parent of directory `id`.
    fn parent_of(&self, id: usize) -> Result<usize, FsError> {
        Ok(self.get(self.directory(id)?)?.parent)
    }

    /// The node a directory `dir` holds under `name`.
    fn child(&self, dir: usize, name: &str) -> Result<usize, FsError> {
        match &self.get(dir)?.body {
            Body::Dir(entries) => entries
                .iter()
                .find(|e| e.name == name)
                .map(|e| e.node)
                .ok_or(FsError::NotFound),
            Body::File(_) => Err(FsError::NotDirectory),
        }
    }

    /// Splits `path` into its parent directory's id and its final name, for operations
    /// that create or remove an entry. The final component must be a name (not `.`/`..`).
    fn split_parent(&self, path: &str) -> Result<(usize, String), FsError> {
        let comps: Vec<Component<'_>> = components(path).ok_or(FsError::BadPath)?.collect();
        let (last, rest) = comps.split_last().ok_or(FsError::BadPath)?;
        let Component::Name(name) = last else {
            return Err(FsError::BadPath);
        };
        let mut dir = Self::ROOT;
        for comp in rest {
            dir = match comp {
                Component::Current => self.directory(dir)?,
                Component::Parent => self.parent_of(dir)?,
                Component::Name(n) => self.child(dir, n)?,
            };
        }
        // The parent must be a directory.
        if !matches!(self.get(dir)?.body, Body::Dir(_)) {
            return Err(FsError::NotDirectory);
        }
        Ok((dir, String::from(*name)))
    }

    /// Allocates a node, reusing a freed slot, or `NoSpace` at the node limit.
    fn alloc(&mut self, node: Node) -> Result<usize, FsError> {
        if self.live_nodes() >= self.max_nodes {
            return Err(FsError::NoSpace);
        }
        if let Some(id) = self.free.pop() {
            self.nodes[id] = Some(node);
            Ok(id)
        } else {
            self.nodes.push(Some(node));
            Ok(self.nodes.len() - 1)
        }
    }

    /// Adds `(name -> node)` to directory `dir`, failing if the name is taken.
    fn link(&mut self, dir: usize, name: String, node: usize) -> Result<(), FsError> {
        match &mut self
            .nodes
            .get_mut(dir)
            .and_then(Option::as_mut)
            .ok_or(FsError::BadHandle)?
            .body
        {
            Body::Dir(entries) => {
                if entries.iter().any(|e| e.name == name) {
                    return Err(FsError::Exists);
                }
                entries.push(Entry { name, node });
                Ok(())
            }
            Body::File(_) => Err(FsError::NotDirectory),
        }
    }

    /// Creates an empty file at `path` and returns its id.
    pub fn create(&mut self, path: &str) -> Result<usize, FsError> {
        let (dir, name) = self.split_parent(path)?;
        if self.child(dir, &name).is_ok() {
            return Err(FsError::Exists);
        }
        let id = self.alloc(Node {
            body: Body::File(Vec::new()),
            parent: dir,
        })?;
        match self.link(dir, name, id) {
            Ok(()) => Ok(id),
            Err(e) => {
                // Roll back the allocation.
                self.nodes[id] = None;
                self.free.push(id);
                Err(e)
            }
        }
    }

    /// Creates a directory at `path` and returns its id.
    pub fn mkdir(&mut self, path: &str) -> Result<usize, FsError> {
        let (dir, name) = self.split_parent(path)?;
        if self.child(dir, &name).is_ok() {
            return Err(FsError::Exists);
        }
        let id = self.alloc(Node {
            body: Body::Dir(Vec::new()),
            parent: dir,
        })?;
        match self.link(dir, name, id) {
            Ok(()) => Ok(id),
            Err(e) => {
                self.nodes[id] = None;
                self.free.push(id);
                Err(e)
            }
        }
    }

    /// Creates every missing directory along `path` (like `mkdir -p`).
    pub fn mkdir_all(&mut self, path: &str) -> Result<usize, FsError> {
        let comps: Vec<Component<'_>> = components(path).ok_or(FsError::BadPath)?.collect();
        let mut here = Self::ROOT;
        for comp in comps {
            here = match comp {
                Component::Current => self.directory(here)?,
                Component::Parent => self.parent_of(here)?,
                Component::Name(name) => match self.child(here, name) {
                    Ok(id) => {
                        if matches!(self.get(id)?.body, Body::File(_)) {
                            return Err(FsError::NotDirectory);
                        }
                        id
                    }
                    Err(FsError::NotFound) => {
                        let id = self.alloc(Node {
                            body: Body::Dir(Vec::new()),
                            parent: here,
                        })?;
                        self.link(here, String::from(name), id)?;
                        id
                    }
                    Err(e) => return Err(e),
                },
            };
        }
        Ok(here)
    }

    /// Writes `data` over file `path` from `offset`, growing it as needed (zero-filling a
    /// gap). Returns the file's new length.
    pub fn write(&mut self, path: &str, offset: usize, data: &[u8]) -> Result<usize, FsError> {
        let id = self.resolve(path)?;
        let max = self.max_file;
        let node = self
            .nodes
            .get_mut(id)
            .and_then(Option::as_mut)
            .ok_or(FsError::BadHandle)?;
        match &mut node.body {
            Body::File(bytes) => {
                let end = offset.checked_add(data.len()).ok_or(FsError::NoSpace)?;
                if end > max {
                    return Err(FsError::NoSpace);
                }
                if bytes.len() < end {
                    bytes.resize(end, 0);
                }
                bytes[offset..end].copy_from_slice(data);
                Ok(bytes.len())
            }
            Body::Dir(_) => Err(FsError::IsDirectory),
        }
    }

    /// Reads up to `out.len()` bytes of file `path` from `offset` into `out`; returns how
    /// many were read (0 at or past the end).
    pub fn read(&self, path: &str, offset: usize, out: &mut [u8]) -> Result<usize, FsError> {
        let id = self.resolve(path)?;
        match &self.get(id)?.body {
            Body::File(bytes) => {
                let start = offset.min(bytes.len());
                let n = out.len().min(bytes.len() - start);
                out[..n].copy_from_slice(&bytes[start..start + n]);
                Ok(n)
            }
            Body::Dir(_) => Err(FsError::IsDirectory),
        }
    }

    /// Truncates or extends file `path` to `len` bytes (zero-filling an extension).
    pub fn truncate(&mut self, path: &str, len: usize) -> Result<(), FsError> {
        if len > self.max_file {
            return Err(FsError::NoSpace);
        }
        let id = self.resolve(path)?;
        match &mut self
            .nodes
            .get_mut(id)
            .and_then(Option::as_mut)
            .ok_or(FsError::BadHandle)?
            .body
        {
            Body::File(bytes) => {
                bytes.resize(len, 0);
                Ok(())
            }
            Body::Dir(_) => Err(FsError::IsDirectory),
        }
    }

    /// Metadata for `path`.
    pub fn stat(&self, path: &str) -> Result<Stat, FsError> {
        let id = self.resolve(path)?;
        let node = self.get(id)?;
        let (kind, size) = match &node.body {
            Body::Dir(entries) => (Kind::Directory, entries.len()),
            Body::File(bytes) => (Kind::File, bytes.len()),
        };
        Ok(Stat { kind, size, id })
    }

    /// The names a directory holds, sorted, or `NotDirectory` for a file.
    pub fn list(&self, path: &str) -> Result<Vec<String>, FsError> {
        let id = self.resolve(path)?;
        match &self.get(id)?.body {
            Body::Dir(entries) => {
                let mut names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
                names.sort();
                Ok(names)
            }
            Body::File(_) => Err(FsError::NotDirectory),
        }
    }

    /// Removes the entry at `path`: a file, or an empty directory. The root cannot be
    /// removed.
    pub fn remove(&mut self, path: &str) -> Result<(), FsError> {
        let (dir, name) = self.split_parent(path)?;
        let id = self.child(dir, &name)?;
        // Refuse a non-empty directory.
        if let Body::Dir(entries) = &self.get(id)?.body
            && !entries.is_empty()
        {
            return Err(FsError::NotEmpty);
        }
        // Unlink from the parent.
        if let Some(Body::Dir(entries)) = self
            .nodes
            .get_mut(dir)
            .and_then(Option::as_mut)
            .map(|n| &mut n.body)
        {
            entries.retain(|e| e.name != name);
        }
        self.nodes[id] = None;
        self.free.push(id);
        Ok(())
    }

    /// Live nodes, the root included (for accounting and the leak check).
    pub fn node_count(&self) -> usize {
        self.live_nodes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comps(path: &str) -> Option<Vec<Component<'_>>> {
        components(path).map(|it| it.collect())
    }

    #[test]
    fn path_parsing() {
        use Component::*;
        assert_eq!(comps("/"), Some(alloc::vec![]));
        assert_eq!(
            comps("/a/b/c"),
            Some(alloc::vec![Name("a"), Name("b"), Name("c")])
        );
        assert_eq!(
            comps("/a//b/"),
            Some(alloc::vec![Name("a"), Name("b")]),
            "empty parts skipped"
        );
        assert_eq!(
            comps("/a/./b/../c"),
            Some(alloc::vec![
                Name("a"),
                Current,
                Name("b"),
                Parent,
                Name("c")
            ])
        );
        assert_eq!(comps(""), None, "empty");
        assert_eq!(comps("a/b"), None, "not absolute");
        let long = alloc::format!("/{}", "x".repeat(MAX_NAME + 1));
        assert_eq!(comps(&long), None, "component too long");
        let ok = alloc::format!("/{}", "x".repeat(MAX_NAME));
        assert!(comps(&ok).is_some());
    }

    #[test]
    fn create_write_read_and_dotdot() {
        let mut fs = Vfs::new(1 << 20, 100);
        fs.mkdir("/etc").unwrap();
        fs.create("/etc/hostname").unwrap();
        let n = fs.write("/etc/hostname", 0, b"origin-os\n").unwrap();
        assert_eq!(n, 10);
        let mut buf = [0u8; 32];
        let got = fs.read("/etc/hostname", 0, &mut buf).unwrap();
        assert_eq!(&buf[..got], b"origin-os\n");
        // `.` and `..` resolve.
        assert_eq!(fs.read("/etc/../etc/./hostname", 0, &mut buf).unwrap(), 10);
        // Partial reads and offsets.
        let got = fs.read("/etc/hostname", 7, &mut buf).unwrap();
        assert_eq!(&buf[..got], b"os\n");
        assert_eq!(
            fs.read("/etc/hostname", 100, &mut buf).unwrap(),
            0,
            "past the end"
        );
        // Overwrite in place and extend with a gap.
        fs.write("/etc/hostname", 0, b"ORIGIN").unwrap();
        let len = fs.write("/etc/hostname", 20, b"!").unwrap();
        assert_eq!(len, 21);
        let got = fs.read("/etc/hostname", 0, &mut buf).unwrap();
        assert_eq!(&buf[..got], b"ORIGIN-os\n\0\0\0\0\0\0\0\0\0\0!");
    }

    /// `.` and `..` need a directory to be relative to: after a file they name nothing, so
    /// neither a lookup nor anything that creates an entry can step through a file.
    #[test]
    fn dot_and_dotdot_after_a_file_are_refused() {
        let mut fs = Vfs::new(1 << 16, 100);
        fs.mkdir("/d").unwrap();
        fs.create("/d/f").unwrap();
        let nodes = fs.node_count();
        for path in ["/d/f/..", "/d/f/.", "/d/f/../f", "/d/f/./"] {
            assert_eq!(fs.resolve(path), Err(FsError::NotDirectory), "{path}");
        }
        assert_eq!(fs.create("/d/f/../g"), Err(FsError::NotDirectory));
        assert_eq!(fs.mkdir("/d/f/../g"), Err(FsError::NotDirectory));
        assert_eq!(fs.create("/d/f/./g"), Err(FsError::NotDirectory));
        assert_eq!(fs.remove("/d/f/../f"), Err(FsError::NotDirectory));
        assert_eq!(fs.mkdir_all("/d/f/../g"), Err(FsError::NotDirectory));
        assert_eq!(
            fs.resolve("/d/g"),
            Err(FsError::NotFound),
            "nothing was created"
        );
        assert_eq!(fs.node_count(), nodes);

        // Through directories they still work, and the root is its own parent.
        assert_eq!(fs.resolve("/d/./f").unwrap(), fs.resolve("/d/f").unwrap());
        assert_eq!(
            fs.resolve("/d/../d/f").unwrap(),
            fs.resolve("/d/f").unwrap()
        );
        assert_eq!(fs.resolve("/../..").unwrap(), fs.resolve("/").unwrap());
        fs.create("/d/../h").unwrap();
        assert_eq!(fs.stat("/h").map(|s| s.kind), Ok(Kind::File));
    }

    #[test]
    fn directories_listing_and_removal() {
        let mut fs = Vfs::new(1 << 16, 100);
        fs.mkdir_all("/a/b/c").unwrap();
        fs.create("/a/b/file1").unwrap();
        fs.create("/a/b/file2").unwrap();
        assert_eq!(fs.list("/a/b").unwrap(), alloc::vec!["c", "file1", "file2"]);
        assert_eq!(fs.stat("/a/b").unwrap().kind, Kind::Directory);
        assert_eq!(fs.stat("/a/b").unwrap().size, 3, "three entries");
        // A non-empty directory cannot be removed.
        assert_eq!(fs.remove("/a/b"), Err(FsError::NotEmpty));
        fs.remove("/a/b/file1").unwrap();
        fs.remove("/a/b/file2").unwrap();
        fs.remove("/a/b/c").unwrap();
        fs.remove("/a/b").unwrap();
        assert_eq!(fs.list("/a").unwrap(), Vec::<String>::new());
        assert_eq!(fs.resolve("/a/b"), Err(FsError::NotFound));
    }

    #[test]
    fn errors_are_distinct() {
        let mut fs = Vfs::new(16, 100);
        fs.mkdir("/d").unwrap();
        fs.create("/f").unwrap();
        assert_eq!(fs.create("/f"), Err(FsError::Exists));
        assert_eq!(fs.mkdir("/d"), Err(FsError::Exists));
        assert_eq!(fs.resolve("/nope"), Err(FsError::NotFound));
        assert_eq!(
            fs.create("/f/under"),
            Err(FsError::NotDirectory),
            "a file is not a directory"
        );
        assert_eq!(fs.read("/d", 0, &mut [0u8; 4]), Err(FsError::IsDirectory));
        assert_eq!(fs.write("/d", 0, b"x"), Err(FsError::IsDirectory));
        assert_eq!(fs.list("/f"), Err(FsError::NotDirectory));
        assert_eq!(
            fs.remove("/"),
            Err(FsError::BadPath),
            "cannot remove the root"
        );
        assert_eq!(fs.create("/"), Err(FsError::BadPath));
        // The file-size limit.
        assert_eq!(fs.write("/f", 0, &[0u8; 17]), Err(FsError::NoSpace));
        fs.write("/f", 0, &[0u8; 16]).unwrap();
        assert_eq!(fs.truncate("/f", 17), Err(FsError::NoSpace));
    }

    #[test]
    fn the_node_limit_and_slot_reuse() {
        let mut fs = Vfs::new(16, 4); // root + 3 more
        fs.create("/a").unwrap();
        fs.create("/b").unwrap();
        fs.create("/c").unwrap();
        assert_eq!(fs.create("/d"), Err(FsError::NoSpace));
        assert_eq!(fs.node_count(), 4);
        // Removing frees a slot for reuse.
        fs.remove("/a").unwrap();
        assert_eq!(fs.node_count(), 3);
        fs.create("/d").unwrap();
        assert_eq!(fs.node_count(), 4);
        // A duplicate name is refused as Exists (checked before allocating), even at the
        // node limit, and leaks nothing.
        assert_eq!(fs.create("/b"), Err(FsError::Exists));
        assert_eq!(fs.node_count(), 4);
    }

    #[test]
    fn a_deterministic_workload_stays_consistent() {
        let mut fs = Vfs::new(1 << 16, 200);
        let mut x = 0x2545_F491u64;
        let mut live_files = alloc::vec::Vec::new();
        for _ in 0..4000 {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let name = alloc::format!("/f{}", x % 50);
            match x >> 60 {
                0..=7 => {
                    if fs.create(&name).is_ok() {
                        live_files.push(name.clone());
                    }
                    // Either way, the path now exists and is a file.
                    assert_eq!(fs.stat(&name).map(|s| s.kind), Ok(Kind::File));
                }
                8..=11 => {
                    let _ = fs.write(&name, (x % 100) as usize, b"data");
                }
                _ => {
                    let _ = fs.remove(&name);
                }
            }
            // The root's children never exceed the files that could exist.
            assert!(fs.list("/").unwrap().len() <= 50);
        }
        // Every listed entry resolves and stats.
        for name in fs.list("/").unwrap() {
            let path = alloc::format!("/{name}");
            assert!(fs.stat(&path).is_ok(), "{path}");
        }
    }
}
