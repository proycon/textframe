/*
TextFrame
  by Maarten van Gompel <proycon@anaproy.nl>
  Digital Infrastructure, KNAW Humanities Cluster
  licensed under the GNU General Public Licence v3
*/

use hmac_sha256::Hash;
use minicbor::{Decode, Encode};
use smallvec::{smallvec, SmallVec};

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom};
use std::ops::Bound::Included;
use std::path::{Path, PathBuf};
use std::string::FromUtf8Error;
use std::time::SystemTime;

/// Handle to a frame (index in a vector)
type FrameHandle = u32;

#[derive(Debug)]
pub enum Error {
    OutOfBoundsError { begin: isize, end: isize },
    EmptyText,
    IOError(std::io::Error),
    Utf8Error(FromUtf8Error),
    InvalidHandle,
    IndexError,
    NotLoaded,
    NoLineIndex,
}

impl fmt::Display for Error {
    /// Formats the error message for printing
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::OutOfBoundsError { begin, end } => write!(f, "Out of Bounds ({},{})", begin, end),
            Self::EmptyText => write!(f, "text is empty"),
            Self::IOError(e) => write!(f, "{}", e),
            Self::Utf8Error(e) => write!(f, "{}", e),
            Self::NotLoaded => write!(f, "text not loaded"),
            Self::InvalidHandle => write!(f, "Invalid handle"),
            Self::IndexError => write!(f, "Index I/O error"),
            Self::NoLineIndex => write!(f, "No line index enabled"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Decode, Encode)]
pub struct PositionData<T>
where
    T: Eq + Ord + Copy,
{
    /// Unicode point
    #[n(0)]
    charpos: T,

    /// UTF-8 byte offset
    #[n(1)]
    bytepos: T,

    /// Size in bytes of this data point and all data points until the next one in the index
    #[n(2)]
    size: u8,
}

pub trait Position {
    fn charpos(&self) -> usize;
    fn bytepos(&self) -> usize;
    fn size(&self) -> u8;
}

impl Position for PositionData<u32> {
    fn charpos(&self) -> usize {
        self.charpos as usize
    }
    fn bytepos(&self) -> usize {
        self.bytepos as usize
    }
    fn size(&self) -> u8 {
        self.size
    }
}

impl Position for PositionData<u64> {
    fn charpos(&self) -> usize {
        self.charpos as usize
    }
    fn bytepos(&self) -> usize {
        self.bytepos as usize
    }
    fn size(&self) -> u8 {
        self.size
    }
}

/// This represent a TextFile and associates a file on disk with
/// immutable excerpts of it (frames) stored in memory.
pub struct TextFile {
    /// The path to the text file
    path: PathBuf,

    /// Holds loaded excerpts of the text (aka 'frames').
    frames: Vec<TextFrame>,

    /// Maps bytes to frame handles (indirection)
    frametable: BTreeMap<usize, SmallVec<[FrameHandle; 1]>>,

    /// Maps character positions to bytes
    positionindex: PositionIndex,

    /// Modification time (unix timestamp)
    metadata: std::fs::Metadata,
}

/// A frame is a fragment of loaded text
struct TextFrame {
    beginbyte: usize,
    endbyte: usize,
    text: String,
}

#[derive(Debug, Clone, Decode, Encode)]
struct PositionIndex {
    /// Length of the text file in characters
    #[n(0)]
    charsize: usize,

    /// Size of the text file in bytes
    #[n(1)]
    bytesize: usize,

    /// Maps character positions to bytes
    #[n(2)]
    positions: Positions,

    /// SHA256 checksum of the contents
    #[n(3)]
    checksum: [u8; 32],

    /// Maps lines to bytes (if enabled)
    #[n(4)]
    lines: Lines,
}

impl Default for PositionIndex {
    fn default() -> Self {
        Self {
            charsize: 0,
            bytesize: 0,
            lines: Lines::default(),
            positions: Positions::Large(Vec::default()),
            checksum: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Decode, Encode)]
/// Abstraction over differently sized position vectors
pub enum Positions {
    #[n(0)]
    Small(#[n(0)] Vec<PositionData<u16>>),

    #[n(1)]
    Large(#[n(0)] Vec<PositionData<u32>>),

    #[n(2)]
    Huge(#[n(0)] Vec<PositionData<u64>>),
}

impl Positions {
    pub fn new(filesize: usize) -> Self {
        if filesize < 65536 {
            Self::Small(Vec::new())
        } else if filesize < 4294967296 {
            Self::Large(Vec::new())
        } else {
            Self::Huge(Vec::new())
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Small(positions) => positions.len(),
            Self::Large(positions) => positions.len(),
            Self::Huge(positions) => positions.len(),
        }
    }

    pub fn bytepos(&self, index: usize) -> Option<usize> {
        match self {
            Self::Small(positions) => positions.get(index).map(|x| x.bytepos as usize),
            Self::Large(positions) => positions.get(index).map(|x| x.bytepos as usize),
            Self::Huge(positions) => positions.get(index).map(|x| x.bytepos as usize),
        }
    }
    pub fn charpos(&self, index: usize) -> Option<usize> {
        match self {
            Self::Small(positions) => positions.get(index).map(|x| x.charpos as usize),
            Self::Large(positions) => positions.get(index).map(|x| x.charpos as usize),
            Self::Huge(positions) => positions.get(index).map(|x| x.charpos as usize),
        }
    }
    pub fn size(&self, index: usize) -> Option<u8> {
        match self {
            Self::Small(positions) => positions.get(index).map(|x| x.size),
            Self::Large(positions) => positions.get(index).map(|x| x.size),
            Self::Huge(positions) => positions.get(index).map(|x| x.size),
        }
    }

    pub fn binary_search(&self, charpos: usize) -> Result<usize, usize> {
        match self {
            Self::Small(positions) => positions
                .binary_search_by_key(&charpos, |posdata: &PositionData<u16>| {
                    posdata.charpos as usize
                }),
            Self::Large(positions) => positions
                .binary_search_by_key(&charpos, |posdata: &PositionData<u32>| {
                    posdata.charpos as usize
                }),
            Self::Huge(positions) => positions
                .binary_search_by_key(&charpos, |posdata: &PositionData<u64>| {
                    posdata.charpos as usize
                }),
        }
    }

    pub fn push(&mut self, charpos: usize, bytepos: usize, charsize: u8) {
        match self {
            Self::Small(positions) => positions.push(PositionData {
                charpos: charpos as u16,
                bytepos: bytepos as u16,
                size: charsize,
            }),
            Self::Large(positions) => positions.push(PositionData {
                charpos: charpos as u32,
                bytepos: bytepos as u32,
                size: charsize,
            }),
            Self::Huge(positions) => positions.push(PositionData {
                charpos: charpos as u64,
                bytepos: bytepos as u64,
                size: charsize,
            }),
        }
    }
}

#[derive(Debug, Clone, Decode, Encode)]
/// Abstraction over differently sized vectors
/// Lines start at 0, the underlying vector contains as many items as there are lines
pub enum Lines {
    #[n(0)]
    Small(#[n(0)] Vec<u16>),

    #[n(1)]
    Large(#[n(0)] Vec<u32>),

    #[n(2)]
    Huge(#[n(0)] Vec<u64>),
}

impl Lines {
    pub fn new(filesize: usize) -> Self {
        if filesize < 65536 {
            Self::Small(Vec::new())
        } else if filesize < 4294967296 {
            Self::Large(Vec::new())
        } else {
            Self::Huge(Vec::new())
        }
    }

    /// Returns the total number of lines
    pub fn len(&self) -> usize {
        match self {
            Self::Small(positions) => positions.len(),
            Self::Large(positions) => positions.len(),
            Self::Huge(positions) => positions.len(),
        }
    }

    /// Returns the byte position where a line begins
    pub fn get(&self, index: usize) -> Option<usize> {
        match self {
            Self::Small(positions) => positions.get(index).map(|x| *x as usize),
            Self::Large(positions) => positions.get(index).map(|x| *x as usize),
            Self::Huge(positions) => positions.get(index).map(|x| *x as usize),
        }
    }

    pub fn push(&mut self, line: usize) {
        match self {
            Self::Small(positions) => positions.push(line as u16),
            Self::Large(positions) => positions.push(line as u32),
            Self::Huge(positions) => positions.push(line as u64),
        }
    }
}

impl Default for Lines {
    fn default() -> Self {
        Self::Large(Vec::new())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
/// Text file mode.
pub enum TextFileMode {
    /// Do not compute a line index (cheapest), set this if you're not interested in line-based queries
    NoLineIndex,

    /// Compute a line index (takes memory and cpu time), allows queries based on line ranges
    WithLineIndex,
}

impl Default for TextFileMode {
    fn default() -> Self {
        Self::WithLineIndex
    }
}

impl TextFile {
    /// Associates with an existing text file on disk, you can optionally provide a path to an indexfile to use for caching the position index. Is such a cache is not available, the text file is scanned once and the index created.

    /// * `path` - The text file
    /// * `indexpath` - The associated index file, acts as a cache if provided to prevent recomputation every time
    /// * `mode` - Additional options
    pub fn new(
        path: impl Into<PathBuf>,
        indexpath: Option<&Path>,
        mode: TextFileMode,
    ) -> Result<Self, Error> {
        let path: PathBuf = path.into();
        let metadata = std::fs::metadata(path.as_path()).map_err(|e| Error::IOError(e))?;
        let mut build_index = true;
        let mut positionindex = PositionIndex::default();
        if let Some(indexpath) = indexpath.as_ref() {
            if indexpath.exists() {
                positionindex = PositionIndex::from_file(indexpath)?;
                build_index = false;
            }
        }
        if build_index {
            positionindex = PositionIndex::new(path.as_path(), metadata.len(), mode)?;
        }
        if let Some(indexpath) = indexpath.as_ref() {
            positionindex.to_file(indexpath)?;
        }
        Ok(Self {
            path,
            frames: Vec::new(),
            frametable: BTreeMap::new(),
            positionindex,
            metadata,
        })
    }

    /// Returns the filename on disk
    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    /// Returns a text fragment. The fragment must already be in memory or an Error::NotLoaded will be returned.
    /// Use `get_or_load()` instead if the fragment might not be loaded yet.
    ///
    /// * `begin` - The begin offset in unicode character points (0-indexed). If negative, it is interpreted relative to the end of the text.
    /// * `end` - The end offset in unicode character points (0-indexed, non-inclusive). If 0 or negative, it is interpreted relative to the end of the text.
    pub fn get(&self, begin: isize, end: isize) -> Result<&str, Error> {
        let (beginchar, endchar) = self.absolute_pos(begin, end)?;
        let beginbyte = self.chars_to_bytes(beginchar)?;
        let endbyte = self.chars_to_bytes(endchar)?;
        self.get_byterange(beginbyte, endbyte)
    }

    pub fn get_byterange(&self, beginbyte: usize, endbyte: usize) -> Result<&str, Error> {
        self.frame(beginbyte, endbyte)
            .ok_or(Error::NotLoaded)
            .map(|frame| {
                &frame.text.as_str()[(beginbyte - frame.beginbyte)..(endbyte - frame.beginbyte)]
            })
    }

    /// Returns a text fragment by lines. The fragment must already be in memory or an Error::NotLoaded will be returned.
    /// Use `get_lines_or_load()` instead if the fragment might not be loaded yet.
    ///
    /// * `begin` - The begin line (0-indexed!!). If negative, it is interpreted relative to the end of the text.
    /// * `end` - The end line (0-indexed!! non-inclusive). If 0 or negative, it is interpreted relative to the end of the text.
    ///
    /// This will return Error::NoLineIndex if no line index was computed.
    /// Trailing newline characters will always be returned.
    pub fn get_lines(&self, begin: isize, end: isize) -> Result<&str, Error> {
        let (beginbyte, endbyte) = self.line_range_to_byte_range(begin, end)?;
        self.get_byterange(beginbyte, endbyte)
    }

    /// Returns a text fragment, the fragment will be loaded from disk into memory if needed.
    /// Use `get()` instead if you are already sure the fragment is loaded
    ///
    /// * `begin` - The begin offset in unicode character points (0-indexed). If negative, it is interpreted relative to the end of the text.
    /// * `end` - The end offset in unicode character points (0-indexed, non-inclusive). If 0 or negative, it is interpreted relative to the end of the text.
    pub fn get_or_load(&mut self, begin: isize, end: isize) -> Result<&str, Error> {
        let (beginchar, endchar) = self.absolute_pos(begin, end)?;
        let beginbyte = self.chars_to_bytes(beginchar)?;
        let endbyte = self.chars_to_bytes(endchar)?;
        match self.framehandle(beginbyte, endbyte) {
            Some(framehandle) => {
                let frame = self.resolve(framehandle)?;
                Ok(
                    &frame.text.as_str()
                        [(beginbyte - frame.beginbyte)..(endbyte - frame.beginbyte)],
                )
            }
            None => {
                self.load_abs(beginchar, endchar)?;
                self.get(begin, end)
            }
        }
    }

    /// Returns a text fragment, the fragment will be loaded from disk into memory if needed.
    /// Use `get_lines()` instead if you are already sure the fragment is loaded
    ///
    /// * `begin` - The begin line (0-indexed!!). If negative, it is interpreted relative to the end of the text.
    /// * `end` - The end line (0-indexed!! non-inclusive). If 0 or negative, it is interpreted relative to the end of the text.
    ///
    /// This will return Error::NoLineIndex if no line index was computed.
    /// Trailing newline characters will always be returned.
    pub fn get_or_load_lines(&mut self, begin: isize, end: isize) -> Result<&str, Error> {
        let beginbyte = self.line_to_bytes(begin)?;
        let endbyte = if end == 0 {
            self.positionindex.bytesize
        } else {
            self.line_to_bytes(end)?
        };
        if let Some(framehandle) = self.framehandle(beginbyte, endbyte) {
            let frame = self.resolve(framehandle)?;
            return Ok(
                &frame.text.as_str()[(beginbyte - frame.beginbyte)..(endbyte - frame.beginbyte)]
            );
        }
        self.load_frame(beginbyte, endbyte)?;
        if let Some(frame) = self.frame(beginbyte, endbyte) {
            Ok(&frame.text.as_str()[(beginbyte - frame.beginbyte)..(endbyte - frame.beginbyte)])
        } else {
            Err(Error::NotLoaded)
        }
    }

    /// Loads a particular text range into memory
    ///
    /// * `begin` - The begin offset in unicode character points (0-indexed). If negative, it is interpreted relative to the end of the text.
    /// * `end` - The end offset in unicode character points (0-indexed, non-inclusive). If 0 or negative, it is interpreted relative to the end of the text.
    pub fn load(&mut self, begin: isize, end: isize) -> Result<(), Error> {
        let (beginchar, endchar) = self.absolute_pos(begin, end)?;
        self.load_abs(beginchar, endchar)
    }

    /// Get a frame from a given handle
    fn resolve(&self, handle: FrameHandle) -> Result<&TextFrame, Error> {
        if let Some(frame) = self.frames.get(handle as usize) {
            Ok(frame)
        } else {
            Err(Error::InvalidHandle)
        }
    }

    /// Returns an existing frame handle that holds the given byte offset (if any is loaded)
    fn framehandle(&self, beginbyte: usize, endbyte: usize) -> Option<FrameHandle> {
        let mut iter = self.frametable.range((Included(&0), Included(&beginbyte)));
        // read the (double-ended) iterator backwards
        // and see if we find a frame that holds the bytes we want
        while let Some((_, framehandles)) = iter.next_back() {
            for handle in framehandles {
                if let Some(frame) = self.frames.get(*handle as usize) {
                    if frame.endbyte >= endbyte {
                        return Some(*handle);
                    }
                }
            }
        }
        None
    }

    /// Returns an existing frame that holds the given byte offset (if any is loaded)
    fn frame(&self, beginbyte: usize, endbyte: usize) -> Option<&TextFrame> {
        let mut iter = self.frametable.range((Included(&0), Included(&beginbyte)));
        // read the (double-ended) iterator backwards
        // and see if we find a frame that holds the bytes we want
        while let Some((_, framehandles)) = iter.next_back() {
            for handle in framehandles {
                if let Some(frame) = self.frames.get(*handle as usize) {
                    if frame.endbyte >= endbyte {
                        return Some(frame);
                    }
                }
            }
        }
        None
    }

    /// Loads a particular text range into memory, takes absolute offsets
    fn load_abs(&mut self, beginchar: usize, endchar: usize) -> Result<(), Error> {
        let beginbyte = self.chars_to_bytes(beginchar)?;
        let endbyte = self.chars_to_bytes(endchar)?;
        match self.load_frame(beginbyte, endbyte) {
            Ok(_handle) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Loads a text frame from disk into memory
    fn load_frame(&mut self, beginbyte: usize, endbyte: usize) -> Result<FrameHandle, Error> {
        if beginbyte > endbyte {
            return Err(Error::OutOfBoundsError {
                begin: beginbyte as isize,
                end: endbyte as isize,
            });
        }
        let mut buffer: Vec<u8> = vec![0; endbyte - beginbyte];
        let mut file = File::open(self.path.as_path()).map_err(|e| Error::IOError(e))?;
        file.seek(SeekFrom::Start(beginbyte as u64))
            .map_err(|e| Error::IOError(e))?;
        file.read_exact(&mut buffer)
            .map_err(|e| Error::IOError(e))?;
        let frame = TextFrame {
            beginbyte,
            endbyte,
            text: String::from_utf8(buffer).map_err(|e| Error::Utf8Error(e))?,
        };
        self.frames.push(frame);
        let handle = (self.frames.len() - 1) as FrameHandle;
        match self.frametable.entry(beginbyte) {
            Entry::Occupied(mut entry) => entry.get_mut().push(handle),
            Entry::Vacant(entry) => {
                entry.insert(smallvec!(handle));
            }
        }
        Ok(handle)
    }

    /// Convert a character position to byte position
    pub fn chars_to_bytes(&self, charpos: usize) -> Result<usize, Error> {
        match self.positionindex.positions.binary_search(charpos) {
            Ok(index) => {
                //exact match
                Ok(self
                    .positionindex
                    .positions
                    .bytepos(index)
                    .expect("position should exist"))
            }
            Err(0) => {
                //insertion before first item should never happen **except if a file is empty**, because the first PositionData item is always the first char
                Err(Error::EmptyText)
            }
            Err(index) => {
                //miss, compute from the item just before, index (>0) will be the item just after the failure
                let charpos2 = self
                    .positionindex
                    .positions
                    .charpos(index - 1)
                    .expect("position should exist");
                let charoffset = charpos - charpos2;
                let bytepos = self
                    .positionindex
                    .positions
                    .bytepos(index - 1)
                    .expect("position should exist")
                    + (self
                        .positionindex
                        .positions
                        .size(index - 1)
                        .expect("position should exist") as usize
                        * charoffset);
                if bytepos > self.positionindex.bytesize {
                    Err(Error::OutOfBoundsError {
                        begin: bytepos as isize,
                        end: 0,
                    })
                } else {
                    Ok(bytepos)
                }
            }
        }
    }

    /// Convert a line number (0-indexed!! first line is 0!) to bytes position.
    /// Relative lines numbers (negative) are supported here as well.
    /// This will return an `Error::IndexError` if no line index was computed/loaded.
    pub fn line_to_bytes(&self, line: isize) -> Result<usize, Error> {
        if self.positionindex.lines.len() == 0 {
            Err(Error::NoLineIndex)
        } else if line < 0 {
            if line.abs() as usize > self.positionindex.lines.len() {
                Err(Error::OutOfBoundsError {
                    begin: line,
                    end: 0,
                })
            } else {
                self.line_to_bytes(self.positionindex.lines.len() as isize - line.abs())
            }
        } else if line as usize == self.positionindex.lines.len() {
            Ok(self.positionindex.bytesize)
        } else {
            if let Some(begin) = self.positionindex.lines.get(line as usize) {
                Ok(begin)
            } else {
                Err(Error::OutOfBoundsError {
                    begin: line,
                    end: 0,
                })
            }
        }
    }

    pub fn line_range_to_byte_range(
        &self,
        begin: isize,
        end: isize,
    ) -> Result<(usize, usize), Error> {
        let beginbyte = self.line_to_bytes(begin)?;
        let endbyte = if end == 0 {
            self.positionindex.bytesize
        } else {
            self.line_to_bytes(end)?
        };

        Ok((beginbyte, endbyte))
    }

    /// Converts relative character offset to an absolute one. If the offset is already absolute, it will be returned as is.
    ///
    /// * `begin` - The begin offset in unicode character points (0-indexed). If negative, it is interpreted relative to the end of the text.
    /// * `end` - The end offset in unicode character points (0-indexed, non-inclusive). If 0 or negative, it is interpreted relative to the end of the text.
    pub fn absolute_pos(&self, begin: isize, end: isize) -> Result<(usize, usize), Error> {
        if begin >= 0 && end > 0 && begin < end {
            Ok((begin as usize, end as usize))
        } else if begin >= 0 && end <= 0 && end.abs() as usize <= self.positionindex.charsize {
            Ok((begin as usize, self.positionindex.charsize + end as usize))
        } else if begin < 0 && end > 0 && begin.abs() as usize <= self.positionindex.charsize {
            let begin_abs = self.positionindex.charsize - begin.abs() as usize;
            if begin_abs > end as usize {
                return Err(Error::OutOfBoundsError { begin, end });
            }
            Ok((begin_abs, end as usize))
        } else if begin < 0
            && end <= 0
            && end.abs() as usize <= self.positionindex.charsize
            && begin.abs() as usize <= self.positionindex.charsize
            && begin.abs() > end.abs()
        {
            let begin_abs = self.positionindex.charsize - begin.abs() as usize;
            let end_abs = self.positionindex.charsize - end.abs() as usize;
            if begin_abs > end_abs {
                return Err(Error::OutOfBoundsError { begin, end });
            }
            Ok((begin_abs, end_abs))
        } else {
            //shouldn't occur
            unreachable!(
                "Out of Bounds with {}-{}, should never happen (logic error)",
                begin, end
            )
        }
    }

    /// Returns the length of the total text file in characters, i.e. the number of character in the text
    pub fn len(&self) -> usize {
        self.positionindex.charsize
    }

    /// Returns the length of the total text file in bytes
    pub fn len_utf8(&self) -> usize {
        self.positionindex.bytesize
    }

    /// Returns the unix timestamp when the file was last modified
    pub fn mtime(&self) -> u64 {
        if let Ok(modified) = self.metadata.modified() {
            modified
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("invalid file timestamp (before unix epoch)")
                .as_secs()
        } else {
            0
        }
    }

    /// Returns the SHA-256 checksum
    pub fn checksum(&self) -> &[u8; 32] {
        &self.positionindex.checksum
    }

    /// Returns the SHA-256 checksum as a digest string
    pub fn checksum_digest(&self) -> String {
        format!("{:x}", HexDigest(self.checksum()))
    }
}

impl PositionIndex {
    /// Build a new positionindex for a given text file
    fn new(textfile: &Path, filesize: u64, options: TextFileMode) -> Result<Self, Error> {
        let mut charpos = 0;
        let mut bytepos = 0;
        let mut prevcharsize = 0;
        let textfile = File::open(textfile).map_err(|e| Error::IOError(e))?;

        // read with a line by line reader to prevent excessive read() syscalls and handle UTF-8 properly
        let mut reader = BufReader::new(textfile);
        let mut positions = Positions::new(filesize as usize);
        let mut lines = Lines::new(filesize as usize);
        let mut line = String::new();
        let mut checksum = Hash::new();
        loop {
            let read_bytes = reader.read_line(&mut line).map_err(|e| Error::IOError(e))?;
            if read_bytes == 0 {
                //EOF
                break;
            } else {
                checksum.update(&line);
                if options == TextFileMode::WithLineIndex {
                    lines.push(bytepos);
                }
                for char in line.chars() {
                    let charsize = char.len_utf8() as u8;
                    if charsize != prevcharsize {
                        positions.push(charpos, bytepos, charsize);
                    }
                    charpos += 1;
                    bytepos += charsize as usize;
                    prevcharsize = charsize;
                }
                //clear buffer for next read
                line.clear();
            }
        }
        let checksum = checksum.finalize();
        if options == TextFileMode::WithLineIndex {
            //the last 'line' marks the end position
            lines.push(bytepos);
        }
        Ok(PositionIndex {
            charsize: charpos,
            bytesize: bytepos,
            positions,
            checksum,
            lines,
        })
    }

    /// Save a positionindex to file
    fn to_file(&mut self, path: &Path) -> Result<(), Error> {
        let file = File::create(path).map_err(|e| Error::IOError(e))?;
        let writer = BufWriter::new(file);
        let writer = minicbor::encode::write::Writer::new(writer);
        minicbor::encode(self, writer).map_err(|_| Error::IndexError)?;
        Ok(())
    }

    /// Load a positionindex from file (quicker than recomputing)
    fn from_file(path: &Path) -> Result<Self, Error> {
        let file = File::open(path).map_err(|e| Error::IOError(e))?;
        let mut reader = BufReader::new(file);
        let mut buffer: Vec<u8> = Vec::new(); //will hold the entire CBOR file!!!
        reader
            .read_to_end(&mut buffer)
            .map_err(|e| Error::IOError(e))?;
        Ok(minicbor::decode(&buffer).map_err(|_| Error::IndexError)?)
    }
}

struct HexDigest<'a>(&'a [u8; 32]);

// You can choose to implement multiple traits, like Lower and UpperHex
impl fmt::LowerHex for HexDigest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{:02x}", byte)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // all single byte-characters, for baseline testing
    const EXAMPLE_ASCII_TEXT: &str = "
Article 1

All human beings are born free and equal in dignity and rights. They are endowed with reason and conscience and should act towards one another in a spirit of brotherhood.

Article 2

Everyone is entitled to all the rights and freedoms set forth in this Declaration, without distinction of any kind, such as race, colour, sex, language, religion, political or other opinion, national or social origin, property, birth or other status. Furthermore, no distinction shall be made on the basis of the political, jurisdictional or international status of the country or territory to which a person belongs, whether it be independent, trust, non-self-governing or under any other limitation of sovereignty.

Article 3

Everyone has the right to life, liberty and security of person.

Article 4

No one shall be held in slavery or servitude; slavery and the slave trade shall be prohibited in all their forms.
";

    // multi-byte characters (mixed with single-byte)
    const EXAMPLE_UNICODE_TEXT: &str = "
第一条

人人生而自由,在尊严和权利上一律平等。他们赋有理性和良心,并应以兄弟关系的精神相对待。
第二条

人人有资格享有本宣言所载的一切权利和自由,不分种族、肤色、性别、语言、宗教、政治或其他见解、国籍或社会出身、财产、出生或其他身分等任何区别。

并且不得因一人所属的国家或领土的政治的、行政的或者国际的地位之不同而有所区别,无论该领土是独立领土、托管领土、非自治领土或者处于其他任何主权受限制的情况之下。
第三条

人人有权享有生命、自由和人身安全。
第四条

任何人不得使为奴隶或奴役;一切形式的奴隶制度和奴隶买卖,均应予以禁止。
";
    const EXAMPLE_3_TEXT: &str = "ПРИВЕТ";

    fn setup_ascii() -> NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        write!(file, "{}", EXAMPLE_ASCII_TEXT).expect("write must work");
        file
    }

    fn setup_unicode() -> NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        write!(file, "{}", EXAMPLE_UNICODE_TEXT).expect("write must work");
        file
    }

    fn setup_3() -> NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        write!(file, "{}", EXAMPLE_3_TEXT).expect("write must work");
        file
    }

    #[test]
    pub fn test001_init_ascii() {
        let file = setup_ascii();
        let textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        assert_eq!(textfile.len(), 914);
        assert_eq!(textfile.len_utf8(), 914);
    }

    #[test]
    pub fn test001_init_unicode() {
        let file = setup_unicode();
        let textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        assert_eq!(textfile.len(), 271);
        assert_eq!(textfile.len_utf8(), 771);
    }

    #[test]
    pub fn test002_load_ascii() {
        let file = setup_ascii();
        let mut textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        let text = textfile.get_or_load(0, 0).expect("text should exist");
        assert_eq!(text, EXAMPLE_ASCII_TEXT);
    }

    #[test]
    pub fn test002_load_ascii_explicit() {
        let file = setup_ascii();
        let mut textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        assert!(textfile.load(0, 0).is_ok());
        let text = textfile.get(0, 0).expect("text should exist");
        assert_eq!(text, EXAMPLE_ASCII_TEXT);
    }

    #[test]
    pub fn test002_load_unicode() {
        let file = setup_unicode();
        let mut textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        let text = textfile.get_or_load(0, 0).expect("text should exist");
        assert_eq!(text, EXAMPLE_UNICODE_TEXT);
    }

    #[test]
    pub fn test002_load_unicode_tiny() {
        let file = setup_3();
        let mut textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        let text = textfile.get_or_load(0, 0).expect("text should exist");
        assert_eq!(text, EXAMPLE_3_TEXT);
    }

    #[test]
    pub fn test003_subpart_of_loaded_frame() {
        let file = setup_ascii();
        let mut textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        assert!(textfile.load(0, 0).is_ok());
        let text = textfile.get(1, 10).expect("text should exist");
        assert_eq!(text, "Article 1");
    }

    #[test]
    pub fn test004_excerpt_in_frame() {
        let file = setup_ascii();
        let mut textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        let text = textfile.get_or_load(1, 10).expect("text should exist");
        assert_eq!(text, "Article 1");
    }

    #[test]
    pub fn test004_end_excerpt_in_frame() {
        let file = setup_ascii();
        let mut textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        let text = textfile.get_or_load(-7, 0).expect("text should exist");
        assert_eq!(text, "forms.\n");
    }

    #[test]
    pub fn test004_excerpt_in_frame_unicode() {
        let file = setup_unicode();
        let mut textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        let text = textfile.get_or_load(1, 4).expect("text should exist");
        assert_eq!(text, "第一条");
    }

    #[test]
    pub fn test004_end_excerpt_in_frame_unicode() {
        let file = setup_unicode();
        let mut textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        let text = textfile.get_or_load(-3, 0).expect("text should exist");
        assert_eq!(text, "止。\n");
    }

    #[test]
    pub fn test005_out_of_bounds() {
        let file = setup_ascii();
        let mut textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        assert!(textfile.load(0, 0).is_ok());
        assert!(textfile.get(1, 999).is_err());
    }

    #[test]
    pub fn test006_checksum() {
        let file = setup_ascii();
        /*
        // compute reference
        let output = std::process::Command::new("sha256sum")
            .arg(file.path())
            .output()
            .expect("Failed to execute command");
        let refsum = String::from_utf8_lossy(&output.stdout).to_owned();
        eprintln!(refsum);
        */
        let textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        assert_eq!(
            textfile.checksum_digest(),
            "c6b079e561f19702d63111a3201d4850e9649b8a3ef1929d6530a780f3815215"
        );
    }

    #[test]
    pub fn test007_positionindex_size() {
        let file = setup_3();
        let mut textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        assert!(textfile.load(0, 0).is_ok());
        assert_eq!(textfile.positionindex.positions.len(), 1);
    }

    #[test]
    pub fn test008_line_ascii() {
        let file = setup_ascii();
        let mut textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        let text = textfile.get_or_load_lines(1, 2).expect("text should exist"); //actual first line is empty in example, this is line 2
        assert_eq!(text, "Article 1\n");
    }

    #[test]
    pub fn test008_empty_line_ascii() {
        let file = setup_ascii();
        let mut textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        let text = textfile.get_or_load_lines(0, 1).expect("text should exist"); //actual first line is empty
        assert_eq!(text, "\n");
    }

    #[test]
    pub fn test008_empty_last_line_ascii() {
        let file = setup_ascii();
        let mut textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        let text = textfile
            .get_or_load_lines(-1, 0)
            .expect("text should exist"); //actual last line is empty in example without trailing newline
        assert_eq!(text, "");
    }

    #[test]
    pub fn test008_empty_last_line() {
        let file = setup_ascii();
        let mut textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        let text = textfile
            .get_or_load_lines(-2, -1)
            .expect("text should exist");
        assert_eq!(text, "No one shall be held in slavery or servitude; slavery and the slave trade shall be prohibited in all their forms.\n");
    }

    #[test]
    pub fn test008_all_lines() {
        let file = setup_unicode();
        let mut textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        assert!(textfile.load(0, 0).is_ok());
        let text = textfile.get_lines(0, 0).expect("text shoulde exist");
        assert_eq!(text, EXAMPLE_UNICODE_TEXT);
    }

    #[test]
    pub fn test009_line_out_of_bounds() {
        let file = setup_ascii();
        let mut textfile =
            TextFile::new(file.path(), None, Default::default()).expect("file must load");
        assert!(textfile.load(0, 0).is_ok());
        assert!(textfile.get_lines(1, 999).is_err());
    }
}
