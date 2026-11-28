//! Blob identities, per-town storage and messages for the blob lane.
//!
//! The store worker owns one town directory exclusively. Both sidecars start with the size
//! (u64 BE) and root (32 bytes), bound to the filename by the blob id. Leaves are 32-byte
//! slots in chunk order. Zero and unwritten slots are absent. A tree holds all levels,
//! leaves through root, with widths derived from the size and the town's chunk length.
//!
//! Recovery table. D is the final data filename, P is `.part`, T is `.tree`, L is `.leaves`.
//! Every nonzero L slot is rehashed against the data. “Check” completes only with all slots
//! present and the signed root reproduced. Otherwise it retains a partial.
//!
//! | Data present | Neither sidecar | L only | T only | T and L |
//! |---|---|---|---|---|
//! | Neither | Nothing | Create P, check | Recover slots from T, check empty P | Check empty P |
//! | P | Identify by hashing P | Check P | Recover slots from T, check P | Check P, redo or undo completion |
//! | D | Identify by hashing D | Check D, finish demotion if partial | Complete with a valid tree header and layout | Check D, finish completion if every slot matches, otherwise demote |
//! | D and P | Identify by hashing the data | Check D | Use D and T | Check D as above |
//!
//! Both data names together are outside the rename protocol. D takes precedence and P can
//! supply a missing match during the check. An unused P beside a complete pair is preserved.
//! With no usable header, a full data file matching its name can rebuild both sidecars.
//! Other data stays partial until `begin` supplies the signed size and root.
//! Across all rows, `.tree.new` is an interrupted tree write. Its bound header and leaves
//! can supply missing metadata before it is discarded or replaced. Interrupted `add-*.tmp`
//! staging files are removed. Unrelated directory entries are left alone.

pub mod lane;
pub mod refs;

use crate::event::{Id, TownId};
use crate::key::label;
use makepad_network::digest::Sha256;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use std::{fmt, ops::Range};

pub const CHUNK: usize = 64 * 1024;
pub type Hash = [u8; 32];

fn digest(parts: &[&[u8]]) -> Hash {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(part);
    }
    hash.finalise()
}

fn leaf(bytes: &[u8]) -> Hash {
    digest(&[label::BLOB_LEAF, bytes])
}

fn node(left: &Hash, right: &Hash) -> Hash {
    digest(&[label::BLOB_NODE, left, right])
}

pub fn id(size: u64, root: &Hash) -> Id {
    digest(&[label::BLOB_ID, &size.to_be_bytes(), root])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyError {
    ZeroChunkLength,
    Index,
    ChunkLength,
    ProofLength,
    RootMismatch,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ZeroChunkLength => "zero blob chunk length",
            Self::Index => "chunk index outside the blob",
            Self::ChunkLength => "wrong byte length for this chunk",
            Self::ProofLength => "wrong proof length for this chunk",
            Self::RootMismatch => "chunk proof does not match the blob root",
        })
    }
}

impl std::error::Error for VerifyError {}

pub struct Tree {
    // Leaves first. The single-hash top level is stored in root.
    levels: Vec<Vec<Hash>>,
    root: Hash,
}

impl Tree {
    pub fn new(bytes: &[u8], chunk_len: usize) -> Result<Self, VerifyError> {
        if chunk_len == 0 {
            return Err(VerifyError::ZeroChunkLength);
        }
        Ok(Self::from_leaves(bytes.chunks(chunk_len).map(leaf).collect()))
    }

    fn from_leaves(mut level: Vec<Hash>) -> Self {
        let mut levels = Vec::new();
        while level.len() > 1 {
            let (pairs, tail) = level.as_chunks::<2>();
            let mut parents = Vec::with_capacity(level.len().div_ceil(2));
            parents.extend(pairs.iter().map(|[left, right]| node(left, right)));
            parents.extend_from_slice(tail);
            levels.push(level);
            level = parents;
        }
        Self { levels, root: level.first().copied().unwrap_or_else(|| leaf(&[])) }
    }

    pub fn root(&self) -> Hash {
        self.root
    }

    /// Siblings from leaf to root, omitting any level that promotes this node.
    pub fn proof(&self, index: u64) -> Result<Vec<Hash>, VerifyError> {
        let mut pos = usize::try_from(index).map_err(|_| VerifyError::Index)?;
        if pos >= self.levels.first().map_or(1, Vec::len) {
            return Err(VerifyError::Index);
        }
        let mut proof = Vec::with_capacity(self.levels.len());
        for level in &self.levels {
            if let Some(sibling) = level.get(pos ^ 1) {
                proof.push(*sibling);
            }
            pos /= 2;
        }
        Ok(proof)
    }
}

// For each sibling the proof must contain, is that sibling on the left?
fn sibling_sides(mut pos: u64, mut width: u64) -> impl Iterator<Item = bool> + Clone {
    std::iter::from_fn(move || {
        while width > 1 {
            let has_sibling = (pos ^ 1) < width;
            let sibling_on_left = pos % 2 == 1;
            width = width.div_ceil(2);
            pos /= 2;
            if has_sibling {
                return Some(sibling_on_left);
            }
        }
        None
    })
}

/// Check a chunk against the signed size and root. The returned hash can go straight into a
/// verified leaf slot, without hashing the chunk twice.
pub fn verify(size: u64, root: &Hash, chunk_len: usize, index: u64, proof: &[Hash], bytes: &[u8])
    -> Result<Hash, VerifyError>
{
    let chunk = chunk_len as u64;
    if chunk == 0 {
        return Err(VerifyError::ZeroChunkLength);
    }
    let width = size.div_ceil(chunk).max(1);
    if index >= width {
        return Err(VerifyError::Index);
    }
    // The index check bounds index * chunk by size, including the sole empty chunk.
    if bytes.len() as u64 != chunk.min(size - index * chunk) {
        return Err(VerifyError::ChunkLength);
    }
    let sides = sibling_sides(index, width);
    if sides.clone().count() != proof.len() {
        return Err(VerifyError::ProofLength);
    }

    let chunk_hash = leaf(bytes);
    let mut hash = chunk_hash;
    for (on_left, sibling) in sides.zip(proof) {
        hash = if on_left { node(sibling, &hash) } else { node(&hash, sibling) };
    }
    if &hash != root {
        return Err(VerifyError::RootMismatch);
    }
    Ok(chunk_hash)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodecError(&'static str);

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for CodecError {}

/// WANT ranges held by the sender, or borrowed directly from a received frame.
#[derive(Clone, Copy, Debug)]
pub enum Ranges<'a> {
    Local(&'a [Range<u64>]),
    Wire(&'a [[[u8; 8]; 2]]),
}

impl<'a> Ranges<'a> {
    pub fn iter(self) -> impl ExactSizeIterator<Item = Range<u64>> + 'a {
        let len = match self { Self::Local(r) => r.len(), Self::Wire(r) => r.len() };
        (0..len).map(move |i| match self {
            Self::Local(r) => r[i].clone(),
            Self::Wire(r) => {
                let [start, end] = r[i];
                u64::from_be_bytes(start)..u64::from_be_bytes(end)
            }
        })
    }

    fn validate(&self) -> Result<(), CodecError> {
        let mut previous_end = 0;
        for range in self.iter() {
            if range.start < previous_end || range.start >= range.end {
                return Err(CodecError("empty, overlapping or out-of-order WANT range"));
            }
            previous_end = range.end;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Body<'a> {
    Want(Ranges<'a>),
    Chunk { index: u64, proof: &'a [Hash], bytes: &'a [u8] },
    Missing,
    Have,
    Ready,
}

#[derive(Clone, Copy, Debug)]
pub struct Message<'a> {
    pub town: TownId,
    pub id: Id,
    pub body: Body<'a>,
}

const MAX_PROOF: usize = u64::BITS as usize;
/// Encoded length of a full CHUNK with the longest proof.
pub const MAX_MESSAGE: usize = 65 + 9 + MAX_PROOF * 32 + CHUNK;

impl<'a> Message<'a> {
    /// Borrow the ranges, proof and bytes. A link worker can drop an unasked chunk before copying it.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, CodecError> {
        let header = CodecError("incomplete blob message header");
        let (&kind, rest) = bytes.split_first().ok_or(header)?;
        let (&town, rest) = rest.split_first_chunk().ok_or(header)?;
        if kind == 5 {
            if !rest.is_empty() { return Err(CodecError("trailing bytes after town READY")); }
            return Ok(Self { town, id: [0; 32], body: Body::Ready });
        }
        let (&id, body) = rest.split_first_chunk().ok_or(header)?;
        let body = match kind {
            1 => {
                let (&count, bytes) = body.split_first_chunk().ok_or(CodecError("missing WANT count"))?;
                let count = u32::from_be_bytes(count) as u64;
                if count * 16 != bytes.len() as u64 {
                    return Err(CodecError("wrong WANT range count"));
                }
                let (words, _) = bytes.as_chunks::<8>();
                let (pairs, _) = words.as_chunks::<2>();
                let ranges = Ranges::Wire(pairs);
                ranges.validate()?;
                Body::Want(ranges)
            }
            2 => {
                let (&index, rest) = body.split_first_chunk().ok_or(CodecError("missing CHUNK index"))?;
                let (&count, rest) = rest.split_first().ok_or(CodecError("missing CHUNK proof length"))?;
                if usize::from(count) > MAX_PROOF {
                    return Err(CodecError("oversized CHUNK proof"));
                }
                let (hashes, bytes) = rest.split_at_checked(usize::from(count) * 32)
                    .ok_or(CodecError("truncated CHUNK proof"))?;
                if bytes.len() > CHUNK {
                    return Err(CodecError("oversized CHUNK data"));
                }
                Body::Chunk { index: u64::from_be_bytes(index), proof: hashes.as_chunks().0, bytes }
            }
            3 if body.is_empty() => Body::Missing,
            4 if body.is_empty() => Body::Have,
            3 | 4 => return Err(CodecError("trailing bytes after blob status")),
            _ => return Err(CodecError("unknown blob message kind")),
        };
        Ok(Self { town, id, body })
    }

    pub fn encode(&self) -> Result<Vec<u8>, CodecError> {
        let (kind, body_len) = match self.body {
            Body::Want(ranges) => {
                ranges.validate()?;
                let count = u32::try_from(ranges.iter().len()).map_err(|_| CodecError("too many WANT ranges"))?;
                (1, 4 + u64::from(count) * 16)
            }
            Body::Chunk { proof, bytes, .. } => {
                if proof.len() > MAX_PROOF || bytes.len() > CHUNK {
                    return Err(CodecError("oversized CHUNK"));
                }
                (2, (9 + proof.len() * 32 + bytes.len()) as u64)
            }
            Body::Missing => (3, 0),
            Body::Have => (4, 0),
            Body::Ready => {
                let mut out = Vec::with_capacity(33);
                out.push(5);
                out.extend_from_slice(&self.town);
                return Ok(out);
            }
        };
        let len = usize::try_from(65 + body_len).map_err(|_| CodecError("blob message too large"))?;
        let mut out = Vec::new();
        out.try_reserve_exact(len).map_err(|_| CodecError("cannot allocate blob message"))?;
        out.push(kind);
        out.extend_from_slice(&self.town);
        out.extend_from_slice(&self.id);
        match self.body {
            Body::Want(ranges) => {
                out.extend_from_slice(&(ranges.iter().len() as u32).to_be_bytes());
                for range in ranges.iter() {
                    out.extend_from_slice(&range.start.to_be_bytes());
                    out.extend_from_slice(&range.end.to_be_bytes());
                }
            }
            Body::Chunk { index, proof, bytes } => {
                out.extend_from_slice(&index.to_be_bytes());
                out.push(proof.len() as u8);
                out.extend_from_slice(proof.as_flattened());
                out.extend_from_slice(bytes);
            }
            Body::Missing | Body::Have | Body::Ready => {}
        }
        Ok(out)
    }
}

const HEADER: u64 = 40;
const ABSENT: Hash = [0; 32];

fn invalid(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

fn read_at(file: &mut File, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
    #[cfg(unix)]
    { std::os::unix::fs::FileExt::read_exact_at(file, bytes, offset) }
    #[cfg(not(unix))]
    { file.seek(SeekFrom::Start(offset))?; file.read_exact(bytes) }
}

fn write_at(file: &mut File, offset: u64, bytes: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    { std::os::unix::fs::FileExt::write_all_at(file, bytes, offset) }
    #[cfg(not(unix))]
    { file.seek(SeekFrom::Start(offset))?; file.write_all(bytes) }
}

fn writable(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(path)
}

fn create_file(path: &Path, exclusive: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if exclusive { options.create_new(true); } else { options.create(true).truncate(true); }
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

fn regular(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => Ok(true),
        Ok(_) => Err(invalid("expected a regular blob file")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

struct Paths {
    data: PathBuf,
    part: PathBuf,
    leaves: PathBuf,
    tree: PathBuf,
    new_tree: PathBuf,
}

/// The verified data file for a blob in its town's store directory.
pub fn data_path(town_dir: &Path, id: &Id) -> PathBuf {
    town_dir.join(crate::hex(id))
}

impl Paths {
    fn new(dir: &Path, id: &Id) -> Self {
        let data = data_path(dir, id);
        Self { part: data.with_extension("part"), leaves: data.with_extension("leaves"),
            tree: data.with_extension("tree"), new_tree: data.with_extension("tree.new"), data }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Info {
    size: u64,
    root: Hash,
}

impl Info {
    fn header(self) -> [u8; 40] {
        let mut bytes = [0; 40];
        bytes[..8].copy_from_slice(&self.size.to_be_bytes());
        bytes[8..].copy_from_slice(&self.root);
        bytes
    }

    fn read(path: &Path, name: &Id) -> io::Result<Option<Self>> {
        let mut file = File::open(path)?;
        let mut size = [0; 8];
        let mut root = [0; 32];
        match file.read_exact(&mut size).and_then(|()| file.read_exact(&mut root)) {
            Ok(()) => {
                let size = u64::from_be_bytes(size);
                Ok((id(size, &root) == *name).then_some(Self { size, root }))
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn count(self, chunk: usize) -> u64 {
        self.size.div_ceil(chunk as u64).max(1)
    }

    fn span(self, chunk: usize, index: u64) -> io::Result<(u64, usize)> {
        if index >= self.count(chunk) { return Err(invalid("chunk index outside the blob")); }
        let start = index * chunk as u64;
        Ok((start, (self.size - start).min(chunk as u64) as usize))
    }
}

fn slot_offset(slot: u64) -> io::Result<u64> {
    slot.checked_mul(32).and_then(|off| off.checked_add(HEADER))
        .ok_or_else(|| invalid("blob slot exceeds the file offset range"))
}

// A sparse partial costs one range per gap, independent of its declared size.
struct Wanted(BTreeMap<u64, u64>);

impl Wanted {
    fn all(count: u64) -> Self {
        Self(BTreeMap::from([(0, count)]))
    }

    fn containing(&self, index: u64) -> Option<(u64, u64)> {
        let (&start, &end) = self.0.range(..=index).next_back()?;
        (index < end).then_some((start, end))
    }

    fn received(&mut self, index: u64) {
        if let Some((start, end)) = self.containing(index) {
            self.0.remove(&start);
            if start < index { self.0.insert(start, index); }
            if index + 1 < end { self.0.insert(index + 1, end); }
        }
    }
}

enum Holding {
    Partial(Wanted),
    Complete,
    // An I/O error interrupted a transition. Its next operation resumes from the files.
    Recover,
}

struct Entry {
    paths: Paths,
    info: Info,
    holding: Holding,
}

/// Counts at the end of an open. Unidentified data has no header bound to its filename yet.
#[derive(Default, Debug)]
pub struct OpenReport {
    pub complete: usize,
    pub partial: usize,
    pub unidentified: usize,
    pub slots_checked: u64,
    pub slots_cleared: u64,
    pub temporary_removed: usize,
    pub interrupted_tree_writes: usize,
    pub completions_finished: usize,
    pub demotions_finished: usize,
    /// Blobs whose tidy failed (a full disk, a read error, a non-file under a blob name).
    /// Their files are left as they are, the rest of the town opens, and `begin` retries them.
    pub failed: Vec<(Id, io::Error)>,
}

pub struct TownStore {
    dir: PathBuf,
    chunk: usize,
    entries: HashMap<Id, Entry>,
    dirty: HashSet<Id>,
    dirty_bytes: u64,
    last_flush: Instant,
}

#[derive(Debug)]
pub enum Served {
    Missing,
    Chunk { proof: Vec<Hash>, bytes: Vec<u8> },
}

/// A failed peer proof can be distinguished from a local write or recovery error.
#[derive(Debug)]
pub enum WriteError {
    Verify(VerifyError),
    Store(io::Error),
}

impl From<io::Error> for WriteError {
    fn from(e: io::Error) -> Self { Self::Store(e) }
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self { Self::Verify(e) => e.fmt(f), Self::Store(e) => e.fmt(f) }
    }
}

impl std::error::Error for WriteError {}

fn unknown() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "unknown town blob")
}

// A short read is a damaged chunk. An I/O error must leave the slot alone for retry.
fn matches_chunk(file: &mut File, offset: u64, buf: &mut [u8], hash: &Hash) -> io::Result<bool> {
    match read_at(file, offset, buf) {
        Ok(()) => Ok(leaf(buf) == *hash),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e),
    }
}

fn buffer(len: usize) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    buf.try_reserve_exact(len).map_err(io::Error::other)?;
    buf.resize(len, 0);
    Ok(buf)
}

// Fill a whole chunk even when the source hands it over in short reads.
fn hash_reader(mut input: impl Read, chunk: usize, mut copy: Option<&mut File>) -> io::Result<(Info, Tree)> {
    let mut buf = buffer(chunk)?;
    let mut hashes = Vec::new();
    let mut size = 0u64;
    loop {
        let mut used = 0;
        while used < chunk {
            match input.read(&mut buf[used..]) {
                Ok(0) => break,
                Ok(n) if n <= chunk - used => used += n,
                Ok(_) => return Err(invalid("reader exceeded its buffer")),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        if used != 0 {
            let bytes = &buf[..used];
            if let Some(out) = copy.as_deref_mut() { out.write_all(bytes)?; }
            hashes.push(leaf(bytes));
            size = size.checked_add(used as u64).ok_or_else(|| invalid("blob exceeds u64 size"))?;
        }
        if used < chunk { break; }
    }
    let tree = Tree::from_leaves(hashes);
    Ok((Info { size, root: tree.root() }, tree))
}

impl Tree {
    fn leaves(&self) -> &[Hash] {
        self.levels.first().map_or(std::slice::from_ref(&self.root), Vec::as_slice)
    }

    fn write(&self, file: &mut File, info: Info) -> io::Result<()> {
        file.write_all(&info.header())?;
        for level in &self.levels { file.write_all(level.as_flattened())?; }
        file.write_all(&self.root)
    }
}

fn temp_file(dir: &Path) -> io::Result<(PathBuf, File)> {
    loop {
        let mut nonce = [0; 16];
        makepad_network::tls::os_random(&mut nonce)?;
        let path = dir.join(format!("add-{}.tmp", crate::hex(&nonce)));
        match create_file(&path, true) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

// An ordinary error cleans up its own staging file. After a crash, open does the same job.
fn with_temp<T>(dir: &Path, write: impl FnOnce(&Path, File) -> io::Result<T>) -> io::Result<T> {
    let (path, file) = temp_file(dir)?;
    let result = write(&path, file);
    let cleanup = remove_if_present(&path);
    let value = result?;
    cleanup?;
    Ok(value)
}

// Publish a replacement sidecar only after all its slots are durable. A failed staging write
// leaves the previous file intact; open removes an interrupted temp.
fn install_leaves(dir: &Path, paths: &Paths, info: Info, hashes: &[Hash]) -> io::Result<()> {
    with_temp(dir, |tmp, mut file| {
        file.write_all(&info.header())?;
        file.write_all(hashes.as_flattened())?;
        file.sync_all()?;
        drop(file);
        fs::rename(tmp, &paths.leaves)?;
        sync_dir(dir)
    })
}

// Read only actual slots, never reserve or pad from the declared size.
fn read_leaves(path: &Path, count: u64) -> io::Result<Option<Tree>> {
    let mut file = File::open(path)?;
    if count > file.metadata()?.len().saturating_sub(HEADER) / 32 { return Ok(None); }
    file.seek(SeekFrom::Start(HEADER))?;
    let mut input = BufReader::new(file);
    let mut hashes = Vec::new();
    let mut hash = [0; 32];
    for _ in 0..count {
        input.read_exact(&mut hash)?;
        if hash == ABSENT { return Ok(None); }
        hashes.push(hash);
    }
    Ok(Some(Tree::from_leaves(hashes)))
}

fn tree_length(mut width: u64) -> io::Result<u64> {
    let mut slots = width;
    while width > 1 {
        width = width.div_ceil(2);
        slots = slots.checked_add(width).ok_or_else(|| invalid("blob tree offset overflow"))?;
    }
    slot_offset(slots)
}

fn tree_layout_ok(path: &Path, info: Info, chunk: usize) -> io::Result<bool> {
    let mut file = File::open(path)?;
    let Ok(len) = tree_length(info.count(chunk)) else { return Ok(false) };
    if file.metadata()?.len() != len { return Ok(false); }
    let mut root = [0; 32];
    read_at(&mut file, len - 32, &mut root)?;
    Ok(root == info.root)
}

// Read just the leaf and the siblings along its path. Check them against the bound root so
// corruption of a sidecar after open cannot authorize a byte or produce a bad proof.
fn tree_path(paths: &Paths, info: Info, chunk: usize, index: u64) -> io::Result<(Hash, Vec<Hash>)> {
    info.span(chunk, index)?;
    let mut file = File::open(&paths.tree)?;
    let mut stored = [0; 32];
    read_at(&mut file, slot_offset(index)?, &mut stored)?;
    let mut width = info.count(chunk);
    let mut proof = Vec::with_capacity(sibling_sides(index, width).count());
    let mut hash = stored;
    let (mut pos, mut base) = (index, 0u64);
    while width > 1 {
        let sibling = pos ^ 1;
        if sibling < width {
            let slot = base.checked_add(sibling).ok_or_else(|| invalid("blob tree offset overflow"))?;
            let mut other = [0; 32];
            read_at(&mut file, slot_offset(slot)?, &mut other)?;
            hash = if pos % 2 == 1 { node(&other, &hash) } else { node(&hash, &other) };
            proof.push(other);
        }
        base = base.checked_add(width).ok_or_else(|| invalid("blob tree offset overflow"))?;
        pos /= 2;
        width = width.div_ceil(2);
    }
    if hash != info.root { return Err(invalid("corrupt blob proof in tree")); }
    Ok((stored, proof))
}


// Check the stored levels before finishing an interrupted unlink of .leaves. An intact
// published tree needs no replacement write (including when the disk is full).
fn same_tree(path: &Path, info: Info, tree: &Tree) -> io::Result<bool> {
    if !regular(path)? { return Ok(false); }
    let mut file = BufReader::new(File::open(path)?);
    let mut scratch = [0; 8192];
    for bytes in std::iter::once(info.header().as_slice())
        .chain(tree.levels.iter().map(|level| level.as_flattened()))
        .chain(std::iter::once(tree.root.as_slice()))
    {
        for block in bytes.chunks(scratch.len()) {
            let buf = &mut scratch[..block.len()];
            match file.read_exact(buf) {
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
                result => result?,
            }
            if buf != block { return Ok(false); }
        }
    }
    Ok(file.read(&mut scratch[..1])? == 0)
}

fn finish(dir: &Path, paths: &Paths, info: Info, source: &Path, tree: &Tree) -> io::Result<()> {
    if tree.root() != info.root { return Err(invalid("blob leaves disagree with the signed root")); }
    if !same_tree(&paths.tree, info, tree)? {
        let mut out = create_file(&paths.new_tree, false)?;
        tree.write(&mut out, info)?;
        out.sync_all()?;
        drop(out);
        fs::rename(&paths.new_tree, &paths.tree)?;
        sync_dir(dir)?;
    }
    let data = writable(source)?;
    let len = data.metadata()?.len();
    if len < info.size { return Err(invalid("complete blob data is too short")); }
    if len > info.size { data.set_len(info.size)?; }
    data.sync_all()?;
    drop(data);
    if source != paths.data {
        fs::rename(source, &paths.data)?;
        sync_dir(dir)?;
    }
    remove_if_present(&paths.leaves)?;
    sync_dir(dir)
}

// Rehash each nonzero on-disk slot. An incomplete last slot is absent; the unwritten tail
// of a sparse fetch need never be visited. Keep the source data even if a read or write fails.
fn check_slots(paths: &Paths, source: &Path, alternate: Option<&Path>, info: Info, chunk: usize,
               report: &mut OpenReport) -> io::Result<Wanted> {
    let count = info.count(chunk);
    let mut wanted = Wanted::all(count);
    let mut data = writable(source)?;
    let mut other = alternate.map(File::open).transpose()?;
    let mut slots = writable(&paths.leaves)?;
    let len = slots.metadata()?.len();
    let whole = (len.saturating_sub(HEADER) / 32).min(count);
    let end = slot_offset(whole)?;
    let mut changed = end != len;
    if changed { slots.set_len(end)?; }
    let mut input = File::open(&paths.leaves)?;
    input.seek(SeekFrom::Start(HEADER))?;
    let mut input = BufReader::new(input);
    let mut buf = if whole == 0 { Vec::new() } else {
        buffer(chunk.min(info.size.try_into().unwrap_or(usize::MAX)))?
    };
    let mut hash = [0; 32];
    for index in 0..whole {
        input.read_exact(&mut hash)?;
        if hash == ABSENT { continue; }
        report.slots_checked += 1;
        let (start, n) = info.span(chunk, index)?;
        let buf = &mut buf[..n];
        let mut good = matches_chunk(&mut data, start, buf, &hash)?;
        if !good {
            if let Some(other) = &mut other {
                if matches_chunk(other, start, buf, &hash)? {
                    write_at(&mut data, start, buf)?;
                    changed = true;
                    good = true;
                }
            }
        }
        if good {
            wanted.received(index);
        } else {
            write_at(&mut slots, slot_offset(index)?, &ABSENT)?;
            report.slots_cleared += 1;
            changed = true;
        }
    }
    if changed {
        data.sync_all()?;
        slots.sync_all()?;
    }
    Ok(wanted)
}

fn recover(dir: &Path, name: &Id, chunk: usize, known: Option<Info>, report: &mut OpenReport)
    -> io::Result<Option<Entry>>
{
    let paths = Paths::new(dir, name);
    let has_data = regular(&paths.data)?;
    let has_part = regular(&paths.part)?;
    let has_leaves = regular(&paths.leaves)?;
    let has_tree = regular(&paths.tree)?;
    let has_new = regular(&paths.new_tree)?;
    if has_new { report.interrupted_tree_writes += 1; }
    let header = |present, path: &Path| {
        if present { Info::read(path, name) } else { Ok(None) }
    };
    let leaves_info = header(has_leaves, &paths.leaves)?;
    let tree_info = header(has_tree, &paths.tree)?;
    let new_info = header(has_new, &paths.new_tree)?;
    let mut info = known.or(leaves_info).or(tree_info).or(new_info);
    // With no bound slots, hash actual data, never a length from a broken header.
    let mut from_data = None;
    if leaves_info.is_none() && tree_info.is_none() && new_info.is_none() {
        for (present, path) in [(has_data, &paths.data), (has_part, &paths.part)] {
            if !present { continue; }
            let (candidate, tree) = hash_reader(File::open(path)?, chunk, None)?;
            if id(candidate.size, &candidate.root) == *name {
                info = Some(candidate);
                from_data = Some(tree);
                break;
            }
        }
    }
    let Some(info) = info else {
        // Even a bare .part can be reused once the signed event supplies its identity.
        if has_tree { fs::remove_file(&paths.tree)?; }
        if has_new { fs::remove_file(&paths.new_tree)?; }
        if has_data && !has_part { fs::rename(&paths.data, &paths.part)?; }
        sync_dir(dir)?;
        return Ok(None);
    };
    let valid_tree = tree_info == Some(info) && tree_layout_ok(&paths.tree, info, chunk)?;
    if has_data && valid_tree && !has_leaves {
        if has_new { fs::remove_file(&paths.new_tree)?; }
        return Ok(Some(Entry { paths, info, holding: Holding::Complete }));
    }

    let source = if has_data { &paths.data } else { &paths.part };
    if !has_data && !has_part { create_file(source, true)?; }
    let count = info.count(chunk);
    if leaves_info.is_none() {
        for (header, path) in [(tree_info, &paths.tree), (new_info, &paths.new_tree)] {
            if from_data.is_none() && header == Some(info) {
                from_data = read_leaves(path, count)?.filter(|t| t.root() == info.root);
            }
        }
        if from_data.is_none() && (has_part || has_data) && (has_tree || has_new) {
            let (candidate, tree) = hash_reader(File::open(source)?, chunk, None)?;
            if candidate == info { from_data = Some(tree); }
        }
        install_leaves(dir, &paths, info, from_data.as_ref().map_or(&[], Tree::leaves))?;
    }
    let alternate = (has_data && has_part).then_some(paths.part.as_path());
    let mut wanted = check_slots(&paths, source, alternate, info, chunk, report)?;
    // A completion tree can reach disk before the last batch of slot writes. With P, use
    // its authenticated leaves to recover those bytes too. With D, a zero is instead the
    // durable demotion decision, even if the old tree is still beside it.
    if !has_data && has_part && leaves_info.is_some() && !wanted.0.is_empty() {
        for (header, path) in [(tree_info, &paths.tree), (new_info, &paths.new_tree)] {
            if header != Some(info) { continue; }
            if let Some(tree) = read_leaves(path, count)?.filter(|tree| tree.root() == info.root) {
                install_leaves(dir, &paths, info, tree.leaves())?;
                wanted = check_slots(&paths, source, None, info, chunk, report)?;
                break;
            }
        }
    }
    // Empty bytes are known locally; no peer message is needed.
    if info.size == 0 && info.root == leaf(&[]) && wanted.containing(0).is_some() {
        install_leaves(dir, &paths, info, &[info.root])?;
        wanted.received(0);
    }
    let mut holding = Holding::Recover;
    if wanted.0.is_empty() {
        if let Some(tree) = read_leaves(&paths.leaves, count)?.filter(|t| t.root() == info.root) {
            finish(dir, &paths, info, source, &tree)?;
            holding = Holding::Complete;
        } else {
            // Self-consistent data and slots with a different aggregate root cannot be localized
            // from those slots. A published tree, if intact, still identifies individual chunks.
            let trusted = if valid_tree {
                read_leaves(&paths.tree, count)?.filter(|t| t.root() == info.root)
            } else { None };
            install_leaves(dir, &paths, info, trusted.as_ref().map_or(&[], Tree::leaves))?;
            wanted = check_slots(&paths, source, alternate, info, chunk, report)?;
            if wanted.0.is_empty() {
                if let Some(tree) = trusted {
                    finish(dir, &paths, info, source, &tree)?;
                    holding = Holding::Complete;
                }
            }
        }
    }
    if !matches!(holding, Holding::Complete) {
        // A partial with the final data name is a torn demotion. Persist the slots before
        // removing the serve marker and moving data, in that order.
        if has_tree || has_data {
            writable(source)?.sync_all()?;
            writable(&paths.leaves)?.sync_all()?;
            if has_tree {
                fs::remove_file(&paths.tree)?;
                sync_dir(dir)?;
            }
            if has_data {
                fs::rename(&paths.data, &paths.part)?;
                sync_dir(dir)?;
                report.demotions_finished += 1;
            }
        }
        holding = Holding::Partial(wanted);
    } else {
        report.completions_finished += 1;
    }
    if has_new { remove_if_present(&paths.new_tree)?; }
    Ok(Some(Entry { paths, info, holding }))
}

impl TownStore {
    /// Open the directory for a single town under the home's blobs directory.
    pub fn open(dir: impl AsRef<Path>, chunk: usize) -> io::Result<(Self, OpenReport)> {
        if chunk == 0 { return Err(invalid("zero blob chunk length")); }
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        if !fs::symlink_metadata(&dir)?.is_dir() { return Err(invalid("expected a blob directory")); }
        let mut names = HashSet::new();
        let mut report = OpenReport::default();
        for item in fs::read_dir(&dir)? {
            let item = item?;
            let filename = item.file_name();
            let Some(filename) = filename.to_str() else { continue };
            if filename.starts_with("add-") && filename.ends_with(".tmp") && item.file_type()?.is_file() {
                // One that can't be removed now is harmless, and the next open tries again.
                if fs::remove_file(item.path()).is_ok() { report.temporary_removed += 1; }
                continue;
            }
            let (name, suffix) = filename.split_once('.').unwrap_or((filename, ""));
            if name.len() != 64 || !name.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || !matches!(suffix, "" | "tree" | "part" | "leaves" | "tree.new")
                || filename.ends_with('.') { continue; }
            if let Some(id) = crate::parse_id(name) { names.insert(id); }
        }
        let mut store = Self { dir, chunk, entries: HashMap::new(), dirty: HashSet::new(),
            dirty_bytes: 0, last_flush: Instant::now() };
        // One blob's failure never closes the town: its files stay as they are.
        for name in names {
            match recover(&store.dir, &name, chunk, None, &mut report) {
                Ok(Some(entry)) => {
                    match entry.holding {
                        Holding::Complete => report.complete += 1,
                        _ => report.partial += 1,
                    }
                    store.entries.insert(name, entry);
                }
                Ok(None) => {
                    let paths = Paths::new(&store.dir, &name);
                    if [&paths.part, &paths.data, &paths.leaves].iter().any(|p| matches!(regular(p), Ok(true))) {
                        report.unidentified += 1;
                        report.partial += 1;
                    }
                }
                Err(e) => report.failed.push((name, e)),
            }
        }
        Ok((store, report))
    }

    fn recover_known(&mut self, name: Id, info: Info) -> io::Result<()> {
        let entry = recover(&self.dir, &name, self.chunk, Some(info), &mut OpenReport::default())?
            .ok_or_else(unknown)?;
        self.entries.insert(name, entry);
        Ok(())
    }

    fn ready(&mut self, name: &Id) -> io::Result<()> {
        if let Some(entry) = self.entries.get(name) {
            if matches!(entry.holding, Holding::Recover) { self.recover_known(*name, entry.info)?; }
        }
        Ok(())
    }

    /// Town-local holdings established by recovery or a successful completion.
    pub fn complete_ids<'a>(&'a self) -> impl Iterator<Item = Id> + 'a {
        self.entries.iter().filter_map(|(id, entry)| matches!(entry.holding, Holding::Complete).then_some(*id))
    }

    pub fn is_complete(&self, id: &Id) -> bool {
        self.entries.get(id).is_some_and(|entry| matches!(entry.holding, Holding::Complete))
    }

    /// Retry one failed startup check. A signed begin or add may already have recovered it.
    pub fn retry_recovery(&mut self, id: Id) -> io::Result<()> {
        if self.entries.contains_key(&id) { return self.ready(&id); }
        if let Some(entry) = recover(&self.dir, &id, self.chunk, None, &mut OpenReport::default())? {
            self.entries.insert(id, entry);
        }
        Ok(())
    }

    pub fn begin(&mut self, size: u64, root: Hash) -> io::Result<Id> {
        let name = id(size, &root);
        if self.entries.contains_key(&name) { self.ready(&name)?; }
        else { self.recover_known(name, Info { size, root })?; }
        Ok(name)
    }

    pub fn absent(&mut self, name: &Id) -> io::Result<Vec<Range<u64>>> {
        self.ready(name)?;
        match &self.entries.get(name).ok_or_else(unknown)?.holding {
            Holding::Partial(wanted) => Ok(wanted.0.iter().map(|(&start, &end)| start..end).collect()),
            Holding::Complete => Ok(Vec::new()),
            Holding::Recover => Err(invalid("blob recovery incomplete")),
        }
    }

    /// Call from the store worker's idle timer as well. A busy transfer also flushes itself.
    pub fn flush(&mut self) -> io::Result<()> {
        for name in &self.dirty {
            let e = self.entries.get_mut(name).ok_or_else(unknown)?;
            if matches!(e.holding, Holding::Recover) {
                *e = recover(&self.dir, name, self.chunk, Some(e.info), &mut OpenReport::default())?
                    .ok_or_else(unknown)?;
            }
            if matches!(e.holding, Holding::Partial(_)) {
                writable(&e.paths.part)?.sync_all()?;
            }
        }
        for name in &self.dirty {
            let e = self.entries.get(name).ok_or_else(unknown)?;
            if matches!(e.holding, Holding::Partial(_)) {
                writable(&e.paths.leaves)?.sync_all()?;
            }
        }
        self.dirty.clear();
        self.dirty_bytes = 0;
        self.last_flush = Instant::now();
        Ok(())
    }

    pub fn write_chunk(&mut self, name: &Id, index: u64, proof: &[Hash], bytes: &[u8]) -> Result<(), WriteError> {
        let info = self.entries.get(name).ok_or_else(unknown)?.info;
        let hash = verify(info.size, &info.root, self.chunk, index, proof, bytes).map_err(WriteError::Verify)?;
        self.ready(name)?;
        let e = self.entries.get_mut(name).ok_or_else(unknown)?;
        let Holding::Partial(wanted) = &mut e.holding else { return Ok(()) };
        let Some(gap) = wanted.containing(index) else { return Ok(()) };
        let last = wanted.0.len() == 1 && gap == (index, index + 1);
        let offset = slot_offset(index)?;
        let (start, _) = info.span(self.chunk, index)?;
        {
            let mut data = writable(&e.paths.part)?;
            let mut slots = writable(&e.paths.leaves)?;
            write_at(&mut data, start, bytes)?;
            write_at(&mut slots, offset, &hash)?;
        }
        if last {
            e.holding = Holding::Recover;
            // Fresh writes already checked the data; only recovery rereads partial data.
            let tree = read_leaves(&e.paths.leaves, info.count(self.chunk))?
                .ok_or_else(|| invalid("missing slots at completion"))?;
            finish(&self.dir, &e.paths, info, &e.paths.part, &tree)?;
            e.holding = Holding::Complete;
            self.dirty.remove(name);
        } else {
            self.dirty.insert(*name);
            self.dirty_bytes = self.dirty_bytes.saturating_add(bytes.len() as u64);
            if self.dirty_bytes >= 4 * 1024 * 1024 || self.last_flush.elapsed() >= Duration::from_secs(1) {
                self.flush()?;
            }
            // A failed automatic sync leaves this slot wanted. Retrying the write also retries
            // that batch; the previous verified slots and any other blob remain intact.
            if let Holding::Partial(wanted) = &mut self.entries.get_mut(name).ok_or_else(unknown)?.holding {
                wanted.received(index);
            }
        }
        Ok(())
    }

    /// Reads only the tree. A damaged or unbound authentication path is an error.
    pub fn proof(&self, name: &Id, index: u64) -> io::Result<Vec<Hash>> {
        let e = self.entries.get(name).ok_or_else(unknown)?;
        if !matches!(e.holding, Holding::Complete) { return Err(unknown()); }
        Ok(tree_path(&e.paths, e.info, self.chunk, index)?.1)
    }

    pub fn serve(&mut self, name: &Id, index: u64) -> io::Result<Served> {
        self.ready(name)?;
        let Some(e) = self.entries.get_mut(name) else { return Ok(Served::Missing) };
        if !matches!(e.holding, Holding::Complete) { return Ok(Served::Missing); }
        let info = e.info;
        let (start, n) = info.span(self.chunk, index)?;
        let mut file = match File::open(&e.paths.data) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                e.holding = Holding::Recover;
                self.ready(name)?;
                return Ok(Served::Missing);
            }
            Err(err) => return Err(err),
        };
        let (hash, proof) = match tree_path(&e.paths, info, self.chunk, index) {
            Ok(path) => path,
            Err(err) if matches!(err.kind(), io::ErrorKind::NotFound | io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData) => {
                // Rebuild a damaged tree from the data if its identity still matches. Otherwise
                // check the remaining slots before allowing the blob to complete again.
                e.holding = Holding::Recover;
                repair_sidecar(&self.dir, &e.paths, info, self.chunk, file)?;
                self.ready(name)?;
                return Ok(Served::Missing);
            }
            Err(err) => return Err(err),
        };
        let mut bytes = buffer(n)?;
        if matches_chunk(&mut file, start, &mut bytes, &hash)? {
            return Ok(Served::Chunk { proof, bytes });
        }
        let paths = &e.paths;
        e.holding = Holding::Recover;
        let count = info.count(self.chunk);
        let Some(mut tree) = read_leaves(&paths.tree, count)?.filter(|t| t.root() == info.root) else {
            repair_sidecar(&self.dir, paths, info, self.chunk, file)?;
            self.ready(name)?;
            return Ok(Served::Missing);
        };
        drop(file);
        let hashes = tree.levels.first_mut().map_or(std::slice::from_mut(&mut tree.root), Vec::as_mut_slice);
        *hashes.get_mut(usize::try_from(index).map_err(|_| invalid("blob index overflow"))?)
            .ok_or_else(|| invalid("missing blob leaf slot"))? = ABSENT;
        install_leaves(&self.dir, paths, info, hashes)?;
        fs::remove_file(&paths.tree)?;
        sync_dir(&self.dir)?;
        fs::rename(&paths.data, &paths.part)?;
        sync_dir(&self.dir)?;
        e.holding = Holding::Partial(Wanted(BTreeMap::from([(index, index + 1)])));
        Ok(Served::Missing)
    }

    pub fn add(&mut self, reader: impl Read) -> io::Result<(u64, Hash)> {
        let dir = self.dir.clone();
        with_temp(&dir, |tmp, mut data| {
            let (info, tree) = hash_reader(reader, self.chunk, Some(&mut data))?;
            data.sync_all()?;
            drop(data);
            let name = self.begin(info.size, info.root)?;
            let e = self.entries.get_mut(&name).ok_or_else(unknown)?;
            let paths = &e.paths;
            e.holding = Holding::Recover;
            install_leaves(&dir, paths, info, tree.leaves())?;
            fs::rename(tmp, &paths.part)?;
            sync_dir(&dir)?;
            finish(&dir, paths, info, &paths.part, &tree)?;
            e.holding = Holding::Complete;
            self.dirty.remove(&name);
            Ok((info.size, info.root))
        })
    }
}


fn repair_sidecar(dir: &Path, paths: &Paths, info: Info, chunk: usize, mut data: File) -> io::Result<()> {
    if regular(&paths.tree)? {
        if let Some(tree) = read_leaves(&paths.tree, info.count(chunk))?.filter(|t| t.root() == info.root) {
            return install_leaves(dir, paths, info, tree.leaves());
        }
    }
    data.seek(SeekFrom::Start(0))?;
    let (actual, tree) = hash_reader(data, chunk, None)?;
    install_leaves(dir, paths, info, if actual == info { tree.leaves() } else { &[] })
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{fields, kind, Content, FieldWriter, Key, Value};
    use crate::log::tests::temp_dir;

    fn assert_serves(store: &mut TownStore, data: &[u8], tree: &Tree) {
        let name = id(data.len() as u64, &tree.root());
        let chunk = store.chunk;
        for i in 0..data.len().div_ceil(chunk).max(1) {
            let Served::Chunk { proof, bytes } = store.serve(&name, i as u64).unwrap()
                else { panic!("missing completed chunk {i}") };
            assert_eq!(bytes, data[i * chunk..data.len().min((i + 1) * chunk)]);
            assert_eq!(proof, tree.proof(i as u64).unwrap());
            assert!(verify(data.len() as u64, &tree.root(), chunk, i as u64, &proof, &bytes).is_ok());
        }
    }

    #[test]
    fn out_of_order_fetch_completes_and_proofs_use_only_the_tree() {
        let dir = temp_dir("blob-fetch");
        let (mut store, _) = TownStore::open(&dir, 4).unwrap();
        let data: Vec<u8> = (0..65).collect();
        let tree = Tree::new(&data, 4).unwrap();
        let name = store.begin(65, tree.root()).unwrap();
        let paths = Paths::new(&dir, &name);
        assert_eq!(store.absent(&name).unwrap(), [0..17]);
        let before = fs::read(&paths.leaves).unwrap();
        assert!(matches!(store.write_chunk(&name, 7, &tree.proof(7).unwrap(), b"nope"),
            Err(WriteError::Verify(VerifyError::RootMismatch))));
        assert_eq!(fs::read(&paths.leaves).unwrap(), before);
        assert_eq!(fs::metadata(&paths.part).unwrap().len(), 0, "invalid proof wrote no data");
        for step in 0..17 {
            let i = (5 + step * 7) % 17;
            let proof = tree.proof(i as u64).unwrap();
            let bytes = &data[i * 4..data.len().min(i * 4 + 4)];
            store.write_chunk(&name, i as u64, &proof, bytes).unwrap();
            if step == 0 {
                store.flush().unwrap();
                store.write_chunk(&name, i as u64, &proof, bytes).unwrap();
                assert_eq!(store.absent(&name).unwrap(), [0..5, 6..17], "duplicate is idempotent");
            }
            if step < 16 { assert!(matches!(store.serve(&name, i as u64).unwrap(), Served::Missing)); }
        }
        assert!(!paths.part.exists() && !paths.leaves.exists());
        assert_serves(&mut store, &data, &tree);
        let hidden = dir.join("withheld-data");
        fs::rename(&paths.data, &hidden).unwrap();
        for i in 0..17 {
            assert_eq!(store.proof(&name, i).unwrap(), tree.proof(i).unwrap(), "proof without data");
        }
        fs::rename(hidden, &paths.data).unwrap();
        let empty = Tree::new(&[], 4).unwrap();
        store.begin(0, empty.root()).unwrap();
        assert_serves(&mut store, &[], &empty);
        assert_eq!(store.add(&[][..]).unwrap(), (0, empty.root()));

        // A hostile signed size does not allocate slots or data at begin or reopen.
        let huge = store.begin(u64::MAX, [7; 32]).unwrap();
        drop(store);
        let (mut store, report) = TownStore::open(&dir, 4).unwrap();
        assert_eq!((report.complete, report.partial, report.slots_checked), (2, 1, 0));
        assert_eq!(store.absent(&huge).unwrap(), [0..u64::MAX.div_ceil(4)]);
        drop(store);

        // A blob that can't be tidied (here a directory under its name) is reported, and the
        // rest of the town still opens and serves.
        let blocked = Paths::new(&dir, &[9; 32]);
        fs::create_dir(&blocked.part).unwrap();
        let (mut store, report) = TownStore::open(&dir, 4).unwrap();
        assert_eq!(report.failed.len(), 1, "the blocked blob is reported");
        assert_eq!(report.failed[0].0, [9; 32]);
        assert_eq!((report.complete, report.partial), (2, 1), "the other blobs still open");
        assert_serves(&mut store, &data, &tree);
        drop(store);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restart_recovers_each_publish_and_demotion_step() {
        let dir = temp_dir("blob-restart");
        let data = b"abcDEFghi!";
        let tree = Tree::new(data, 3).unwrap();
        let info = Info { size: data.len() as u64, root: tree.root() };
        let name = id(info.size, &info.root);
        let slots = [info.header().as_slice(), tree.leaves().as_flattened()].concat();
        let (mut seed, _) = TownStore::open(dir.join("seed"), 3).unwrap();
        assert_eq!(seed.add((&data[..2]).chain(&data[2..])).unwrap(), (info.size, info.root));
        let sidecar = fs::read(Paths::new(&seed.dir, &name).tree).unwrap();
        drop(seed);
        // Cross the four persistent names with .tree.new. This includes the cuts before and
        // after every rename and unlink, and the intact-data branch of the ambiguous state.
        for mask in 0..32 {
            let town = dir.join(mask.to_string());
            fs::create_dir(&town).unwrap();
            let p = Paths::new(&town, &name);
            for (bit, path, bytes) in [(1, &p.data, &data[..]), (2, &p.part, data),
                (4, &p.leaves, &slots), (8, &p.tree, &sidecar), (16, &p.new_tree, &sidecar)] {
                if mask & bit != 0 { fs::write(path, bytes).unwrap(); }
            }
            let (mut store, _) = TownStore::open(&town, 3).unwrap();
            store.begin(info.size, info.root).unwrap();
            if mask & 3 != 0 {
                assert!(store.absent(&name).unwrap().is_empty(), "complete, mask {mask}");
                assert_serves(&mut store, data, &tree);
                assert!(!p.leaves.exists());
            } else {
                assert_eq!(store.absent(&name).unwrap(), [0..4], "absent data, mask {mask}");
                assert!(matches!(store.serve(&name, 0).unwrap(), Served::Missing));
                assert!(p.part.exists() && !p.tree.exists());
            }
            assert!(!p.new_tree.exists());
        }
        // Cut demotion immediately after each of its three steps. The data mismatch also
        // resolves D+T+L when the stale slot is nonzero.
        for cut in 0..4 {
            let town = dir.join(format!("demote-{cut}"));
            fs::create_dir(&town).unwrap();
            let p = Paths::new(&town, &name);
            let mut damaged = *data;
            damaged[4] ^= 1;
            fs::write(if cut == 3 { &p.part } else { &p.data }, damaged).unwrap();
            if cut <= 1 { fs::write(&p.tree, &sidecar).unwrap(); }
            let mut disk = slots.clone();
            if cut != 0 { disk[72..104].fill(0); }
            fs::write(&p.leaves, disk).unwrap();
            let (mut store, report) = TownStore::open(&town, 3).unwrap();
            assert_eq!((report.complete, report.partial), (0, 1));
            assert_eq!(store.absent(&name).unwrap(), [1..2], "damaged chunk at demotion cut {cut}");
            assert!(matches!(store.serve(&name, 1).unwrap(), Served::Missing));
            assert!(p.part.exists() && p.leaves.exists() && !p.tree.exists() && !p.data.exists());
            store.write_chunk(&name, 1, &tree.proof(1).unwrap(), &data[3..6]).unwrap();
            assert_serves(&mut store, data, &tree);
        }

        let town = dir.join("interrupted-arrival");
        fs::create_dir(&town).unwrap();
        let p = Paths::new(&town, &name);
        fs::write(&p.part, &data[..9]).unwrap(); // the last slot has no data
        let mut disk = slots.clone();
        disk[40..72].fill(0); // first chunk has data without a slot
        disk[104..136].fill(0x77); // third slot is garbage
        fs::write(&p.leaves, disk).unwrap();
        fs::write(&p.new_tree, &sidecar[..19]).unwrap();
        fs::write(town.join("add-unfinished.tmp"), b"scratch").unwrap();
        fs::write(town.join("other.tmp"), b"keep").unwrap();
        let (mut store, report) = TownStore::open(&town, 3).unwrap();
        assert_eq!((report.slots_checked, report.slots_cleared, report.temporary_removed), (3, 2, 1));
        assert_eq!(store.absent(&name).unwrap(), [0..1, 2..4]);
        assert!(town.join("other.tmp").exists() && !p.new_tree.exists());
        for i in [2usize, 3, 0] {
            store.write_chunk(&name, i as u64, &tree.proof(i as u64).unwrap(),
                &data[i * 3..data.len().min(i * 3 + 3)]).unwrap();
        }
        assert_serves(&mut store, data, &tree);
        // The tree rename can persist before the last unsynced slot. Recover that chunk from
        // P using the tree. A zero beside D instead commits demotion, even for intact bytes.
        drop(store);
        fs::write(&p.leaves, &slots[..slots.len() - 32]).unwrap();
        let (mut store, _) = TownStore::open(&town, 3).unwrap();
        assert_eq!(store.absent(&name).unwrap(), [3..4], "zero slot beside final data demotes");
        drop(store);
        fs::write(&p.tree, &sidecar).unwrap();
        let (mut store, _) = TownStore::open(&town, 3).unwrap();
        assert_serves(&mut store, data, &tree);
        drop(store);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_complete_chunk_is_demoted_and_repair_is_retryable() {
        let dir = temp_dir("blob-demote");
        let (mut store, _) = TownStore::open(&dir, 3).unwrap();
        let data = b"abcDEFghi!";
        let tree = Tree::new(data, 3).unwrap();
        let (size, root) = store.add(&data[..]).unwrap();
        let name = id(size, &root);
        let p = Paths::new(&dir, &name);
        write_at(&mut writable(&p.data).unwrap(), 7, b"?").unwrap();
        assert!(matches!(store.serve(&name, 2).unwrap(), Served::Missing));
        assert_eq!(store.absent(&name).unwrap(), [2..3]);
        assert!(p.part.exists() && p.leaves.exists() && !p.tree.exists() && !p.data.exists());
        assert!(matches!(store.serve(&name, 0).unwrap(), Served::Missing), "partial never serves");
        let (mut store, _) = TownStore::open(&dir, 3).unwrap();
        assert_eq!(store.absent(&name).unwrap(), [2..3]);
        // A storage error before the slot write preserves both the wanted range and earlier
        // verified bytes. A failed local import also cleans up its own scratch.
        let previous = fs::read(&p.part).unwrap();
        let saved_slots = dir.join("saved-slots");
        fs::rename(&p.leaves, &saved_slots).unwrap();
        fs::create_dir(&p.leaves).unwrap();
        assert!(matches!(store.write_chunk(&name, 2, &tree.proof(2).unwrap(), b"ghi"), Err(WriteError::Store(_))));
        fs::remove_dir(&p.leaves).unwrap();
        fs::rename(saved_slots, &p.leaves).unwrap();
        assert_eq!(fs::read(&p.part).unwrap(), previous);
        struct FailedReader;
        impl Read for FailedReader {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { Err(io::Error::other("interrupted import")) }
        }
        assert!(store.add((&data[..3]).chain(FailedReader)).is_err());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2, "only the partial and its slots remain");
        assert_eq!(store.absent(&name).unwrap(), [2..3]);
        // Force the tree write to fail. The verified data and slots survive and retry publishes
        // the same blob without fetching any other chunk.
        fs::create_dir(&p.new_tree).unwrap();
        assert!(matches!(store.write_chunk(&name, 2, &tree.proof(2).unwrap(), b"ghi"), Err(WriteError::Store(_))));
        assert!(!p.data.exists() && !p.tree.exists());
        assert_eq!(fs::read(&p.part).unwrap(), data);
        fs::remove_dir(&p.new_tree).unwrap();
        store.write_chunk(&name, 2, &tree.proof(2).unwrap(), b"ghi").unwrap();
        assert_serves(&mut store, data, &tree);
        // A changed leaf and matching changed data cannot authenticate each other.
        write_at(&mut writable(&p.data).unwrap(), 0, b"xyz").unwrap();
        write_at(&mut writable(&p.tree).unwrap(), HEADER, &leaf(b"xyz")).unwrap();
        assert!(store.proof(&name, 0).is_err());
        assert!(matches!(store.serve(&name, 0).unwrap(), Served::Missing));
        assert!(!p.tree.exists() && !p.data.exists());
        drop(store);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tree_vectors_and_every_proof() {
        // Independent SHA-256 vectors. Three bytes per chunk, with a two-byte final chunk.
        for (n, expected) in [
            (0usize, "59d4ec551f90a142c9113360247f1ae4fd742f97fd0b864774dfc04dd357e3e8"),
            (1, "d70b736939783cb6baf04edc0af1b1a5d5d43599f3704e94f7584a3ba5524bcc"),
            (2, "b9bffacebdd9c1eaa419c8fbd65da4d55acf40118091ce857923e382ce622a4f"),
            (3, "3bf107603b0a8cd28ff7bd92b93fe12c8b0ae8840db38271402ee73aa90ed48c"),
            (5, "f2c72abb582bef3d71968a48142be4d43e577627f6abb6dbb98ca8490074c898"),
            (1000, "775a1b4ca14ff331b4d59d3737627d187dbdb945e3b01709efd74fa80fbe8724"),
        ] {
            let len = (n * 3).saturating_sub(1);
            let bytes: Vec<_> = (0..len).map(|i| (i % 251) as u8).collect();
            let tree = Tree::new(&bytes, 3).unwrap();
            let root = tree.root();
            let size = len as u64;
            assert_eq!(crate::hex(&root), expected, "{n} chunks");
            assert_ne!(id(size, &root), id(size + 1, &root), "size is bound into the id");
            if n == 1000 {
                assert_eq!(crate::hex(&id(size, &root)), "843693b7e2fb433a4d644d777901c1f978c773a5c0f167fccc1e4c20468969cc");
            }
            for i in 0..n.max(1) {
                let chunk = &bytes[i * 3..len.min(i * 3 + 3)];
                let proof = tree.proof(i as u64).unwrap();
                assert_eq!(verify(size, &root, 3, i as u64, &proof, chunk), Ok(leaf(chunk)), "chunk {i} of {n}");
            }
            assert_eq!(tree.proof(n.max(1) as u64), Err(VerifyError::Index));
        }
    }

    #[test]
    fn verifier_refuses_bad_shapes_lengths_and_hashes() {
        let tree = Tree::new(b"abcDEFghiJKL!", 3).unwrap();
        let root = tree.root();
        let proof = tree.proof(1).unwrap();
        let check = |i, proof: &[Hash], bytes: &[u8]| verify(13, &root, 3, i, proof, bytes);
        let mut wrong = proof.clone();
        wrong[0][7] ^= 1;
        assert_eq!(check(1, &wrong, b"DEF"), Err(VerifyError::RootMismatch));
        assert_eq!(check(0, &proof, b"DEF"), Err(VerifyError::RootMismatch));
        assert_eq!(check(1, &proof[1..], b"DEF"), Err(VerifyError::ProofLength));
        let mut long = proof.clone();
        long.push(root);
        assert_eq!(check(1, &long, b"DEF"), Err(VerifyError::ProofLength));
        for short_or_long in [&b"DE"[..], b"DEF!", b"D"] {
            assert_eq!(check(1, &proof, short_or_long), Err(VerifyError::ChunkLength));
        }
        let last = tree.proof(4).unwrap();
        assert_eq!(last.len(), 1, "two odd levels promote the last chunk");
        for bytes in [&b""[..], b"!!"] {
            assert_eq!(check(4, &last, bytes), Err(VerifyError::ChunkLength));
        }
        for i in [5, u64::MAX] {
            assert_eq!(check(i, &[], b""), Err(VerifyError::Index));
        }
        assert_eq!(verify(0, &root, 0, 0, &[], b""), Err(VerifyError::ZeroChunkLength));
        assert!(matches!(Tree::new(b"", 0), Err(VerifyError::ZeroChunkLength)));
        assert_eq!(verify(u64::MAX, &root, 2, u64::MAX / 2, &[], b"x"), Err(VerifyError::ProofLength));
        assert_eq!(verify(3, &leaf(b"abc"), 3, 0, &[], b"abc"), Ok(leaf(b"abc")), "full final chunk");
    }

    #[test]
    fn signed_blob_reference_has_a_big_endian_size_and_root() {
        let root = [0x55; 32];
        let size = 0x0102_0304_0506_0708;
        let content = Content::Entity { copied_from: vec![],
            kind: Key::Name("Card".into()), project: None, derive: None,
            fields: vec![(Key::Number(1), Value::Blob { size, root })],
        };
        let wire = content.encode();
        let mut tagged = vec![1, 2, 3, 4, 5, 6, 7, 8];
        tagged.extend(root);
        let entry = fields(&wire).unwrap().into_iter().find(|f| f.0 == 3).unwrap().1;
        assert!(fields(entry).unwrap().contains(&(9, tagged.as_slice())));
        assert_eq!(Content::decode(kind::ENTITY, content.version(), &wire), Some(content));

        let old_field = FieldWriter::default().bytes(1, &1u16.to_le_bytes()).bytes(9, &root).finish();
        let old = FieldWriter::default().text(5, "Card").bytes(3, &old_field).finish();
        assert_eq!(Content::decode(kind::ENTITY, 3, &old), None, "32 bytes cannot decode as a blob");
    }

    #[test]
    fn lane_message_vectors_and_malformed_frames() {
        let town = [0x31; 32];
        let id = [0x42; 32];
        let header = |kind| [&[kind][..], &town, &id].concat();
        let encode = |body| Message { town, id, body }.encode().unwrap();

        for (kind, body) in [(3, Body::Missing), (4, Body::Have)] {
            let mut wire = encode(body);
            assert_eq!(wire, header(kind));
            assert_eq!(Message::decode(&wire).unwrap().encode().unwrap(), wire);
            for cut in 0..wire.len() {
                assert!(Message::decode(&wire[..cut]).is_err(), "status header cut at {cut}");
            }
            wire.push(0);
            assert!(Message::decode(&wire).is_err(), "status with trailing byte");
        }
        assert!(Message::decode(&header(0)).is_err(), "unknown kind");

        let ready = encode(Body::Ready);
        assert_eq!(ready, [&[5][..], &town].concat());
        assert!(matches!(Message::decode(&ready).unwrap().body, Body::Ready));
        assert!(Message::decode(&ready[..32]).is_err() && Message::decode(&header(5)).is_err());

        let ranges = [0..2, 2..0x102, 800..u64::MAX];
        let mut want = encode(Body::Want(Ranges::Local(&ranges)));
        let mut expected = header(1);
        expected.extend([0, 0, 0, 3]);
        for end in [0u64, 2, 2, 0x102, 800, u64::MAX] {
            expected.extend(end.to_be_bytes());
        }
        assert_eq!(want, expected);
        let message = Message::decode(&want).unwrap();
        let Body::Want(decoded) = message.body else { panic!("expected WANT") };
        assert!(decoded.iter().eq(ranges.iter().cloned()));
        assert_eq!(message.encode().unwrap(), want);
        for cut in 0..want.len() {
            assert!(Message::decode(&want[..cut]).is_err(), "WANT cut at {cut}");
        }
        want.push(0);
        assert!(Message::decode(&want).is_err(), "WANT with trailing byte");

        let cancel = encode(Body::Want(Ranges::Local(&[])));
        assert_eq!(cancel[65..], [0; 4]);
        assert_eq!(Message::decode(&cancel).unwrap().encode().unwrap(), cancel);
        for count in [1u32, u32::MAX] {
            let mut bad = header(1);
            bad.extend(count.to_be_bytes());
            assert!(Message::decode(&bad).is_err(), "count without ranges");
        }
        for (start, end) in [(4u64, 4u64), (8, 4), (3, 9), (0, 1)] {
            let mut bad = header(1);
            bad.extend(2u32.to_be_bytes());
            for value in [1u64, 4, start, end] {
                bad.extend(value.to_be_bytes());
            }
            assert!(Message::decode(&bad).is_err(), "invalid range {start}..{end} after 1..4");
        }

        let proof = [[0x17; 32]; MAX_PROOF];
        let data = vec![0x54; CHUNK];
        let index = 0x0102_0304_0506_0708;
        let mut wire = encode(Body::Chunk { index, proof: &proof, bytes: &data });
        assert_eq!(wire.len(), MAX_MESSAGE, "a full CHUNK with the longest proof fits exactly");
        assert_eq!(&wire[..65], &header(2));
        assert_eq!(wire[65..74], [1, 2, 3, 4, 5, 6, 7, 8, 64]);
        let message = Message::decode(&wire).unwrap();
        let Body::Chunk { index: got, proof: siblings, bytes } = message.body else { panic!("expected CHUNK") };
        assert_eq!(got, index);
        assert_eq!(siblings, &proof);
        assert_eq!(bytes, &data);
        assert_eq!(siblings.as_flattened().as_ptr(), wire[74..].as_ptr(), "proof borrows the message");
        assert_eq!(bytes.as_ptr(), wire[74 + MAX_PROOF * 32..].as_ptr(), "data borrows the message");
        assert_eq!(message.encode().unwrap(), wire);
        let data_start = wire.len() - CHUNK;
        for cut in 0..data_start {
            assert!(Message::decode(&wire[..cut]).is_err(), "CHUNK header or proof cut at {cut}");
        }
        // The data consumes the rest. Its exact blob-specific length is checked by verify.
        assert!(Message::decode(&wire[..data_start]).is_ok());
        wire.push(0);
        assert!(Message::decode(&wire).is_err(), "one byte past a full chunk");
        wire.pop();
        wire[73] = 65;
        assert!(Message::decode(&wire).is_err(), "oversized proof even with enough bytes");
    }
}
