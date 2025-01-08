/*
TextFrame
  by Maarten van Gompel <proycon@anaproy.nl>
  Digital Infrastructure, KNAW Humanities Cluster
  licensed under the GNU General Public Licence v3
*/

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

/// Handle to a frame (index in a vector)
type FrameHandle = u32;

#[derive(Debug)]
pub enum Error {
    OutOfBoundsError { begin: isize, end: isize },
    IOError(std::io::Error),
    Utf8Error(FromUtf8Error),
    InvalidHandle,
    IndexError,
    NotLoaded,
}

impl fmt::Display for Error {
    /// Formats the error message for printing
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::OutOfBoundsError { begin, end } => write!(f, "Out of Bounds ({},{})", begin, end),
            Self::IOError(e) => write!(f, "{}", e),
            Self::Utf8Error(e) => write!(f, "{}", e),
            Self::NotLoaded => write!(f, "text not loaded"),
            Self::InvalidHandle => write!(f, "Invalid handle"),
            Self::IndexError => write!(f, "Index I/O error"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Decode, Encode)]
pub struct PositionData {
    /// Unicode point
    #[n(0)]
    charpos: usize,

    /// UTF-8 byte offset
    #[n(1)]
    bytepos: usize,

    /// Size in bytes of this data point and all data points until the next one in the index
    #[n(2)]
    size: u8,
}

impl PositionData {
    pub fn charpos(&self) -> usize {
        self.charpos
    }
    pub fn bytepos(&self) -> usize {
        self.bytepos
    }
    pub fn size(&self) -> u8 {
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
}

/// A frame is a fragment of loaded text
struct TextFrame {
    beginbyte: usize,
    endbyte: usize,
    text: String,
}

#[derive(Debug, Default, Clone, Decode, Encode)]
struct PositionIndex {
    /// Length of the text file in characters
    #[n(0)]
    charsize: usize,

    /// Size of the text file in bytes
    #[n(1)]
    bytesize: usize,

    /// Maps character positions to bytes
    #[n(2)]
    positions: Vec<PositionData>,
}

impl TextFile {
    /// Associates with an existing text file on disk, you can optionally provide an path to an indexfile to use for caching the position index. Is such a cache is not available, the text file is scanned once and the index created.
    pub fn new(path: impl Into<PathBuf>, indexpath: Option<&Path>) -> Result<Self, Error> {
        let mut textfile = Self {
            path: path.into(),
            frames: Vec::new(),
            frametable: BTreeMap::new(),
            positionindex: PositionIndex::default(),
        };

        let mut build_index = true;
        if let Some(indexpath) = indexpath.as_ref() {
            if indexpath.exists() {
                textfile.positionindex = PositionIndex::from_file(indexpath)?;
                build_index = false;
            }
        }
        if build_index {
            textfile.positionindex = PositionIndex::new(textfile.path.as_path())?;
        }
        if let Some(indexpath) = indexpath.as_ref() {
            textfile.positionindex.to_file(indexpath)?;
        }
        Ok(textfile)
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
        if let Some(frame) = self.frame(beginbyte, endbyte) {
            Ok(&frame.text.as_str()[(beginbyte - frame.beginbyte)..(endbyte - frame.beginbyte)])
        } else {
            Err(Error::NotLoaded)
        }
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
        if let Some(framehandle) = self.framehandle(beginbyte, endbyte) {
            let frame = self.resolve(framehandle)?;
            return Ok(
                &frame.text.as_str()[(beginbyte - frame.beginbyte)..(endbyte - frame.beginbyte)]
            );
        }
        self.load_abs(beginchar, endchar)?;
        self.get(begin, end)
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
        match self
            .positionindex
            .positions
            .binary_search_by_key(&charpos, |posdata: &PositionData| posdata.charpos)
        {
            Ok(index) => {
                //exact match
                let posdata = self
                    .positionindex
                    .positions
                    .get(index)
                    .expect("position should exist");
                Ok(posdata.bytepos())
            }
            Err(0) => {
                //insertion before first item should never happen , because the first PositionData item is always the first char
                unreachable!("match before first positiondata should not occur")
            }
            Err(index) => {
                //miss, compute from the item just before, index (>0) will be the item just after the failure
                let posdata = self
                    .positionindex
                    .positions
                    .get(index - 1)
                    .expect("position should exist");
                let charoffset = charpos - posdata.charpos();
                let bytepos = posdata.bytepos() + (posdata.size() as usize * charoffset);
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
}

impl PositionIndex {
    /// Build a new positionindex for a given text file
    fn new(textfile: &Path) -> Result<Self, Error> {
        let mut charpos = 0;
        let mut bytepos = 0;
        let mut prevcharsize = 0;
        let mut positions: Vec<PositionData> = Vec::new();
        let textfile = File::open(textfile).map_err(|e| Error::IOError(e))?;
        // read with a line by line to prevent excessive read() syscalls and handle UTF-8 properly
        let mut reader = BufReader::new(textfile);
        let mut line = String::new();
        loop {
            let read_bytes = reader.read_line(&mut line).map_err(|e| Error::IOError(e))?;
            if read_bytes == 0 {
                //EOF
                break;
            } else {
                for char in line.chars() {
                    let charsize = char.len_utf8() as u8;
                    if charsize != prevcharsize {
                        positions.push(PositionData {
                            charpos,
                            bytepos,
                            size: charsize,
                        });
                    }
                    charpos += 1;
                    bytepos += charsize as usize;
                    prevcharsize = charsize;
                }
                //clear buffer for next read
                line.clear();
            }
        }
        Ok(PositionIndex {
            charsize: charpos,
            bytesize: bytepos,
            positions,
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

    #[test]
    pub fn test001_init_ascii() {
        let file = setup_ascii();
        let textfile = TextFile::new(file.path(), None).expect("file must load");
        assert_eq!(textfile.len(), 914);
        assert_eq!(textfile.len_utf8(), 914);
    }

    #[test]
    pub fn test001_init_unicode() {
        let file = setup_unicode();
        let textfile = TextFile::new(file.path(), None).expect("file must load");
        assert_eq!(textfile.len(), 271);
        assert_eq!(textfile.len_utf8(), 771);
    }

    #[test]
    pub fn test002_load_ascii() {
        let file = setup_ascii();
        let mut textfile = TextFile::new(file.path(), None).expect("file must load");
        let text = textfile.get_or_load(0, 0).expect("text should exist");
        assert_eq!(text, EXAMPLE_ASCII_TEXT);
    }

    #[test]
    pub fn test002_load_ascii_explicit() {
        let file = setup_ascii();
        let mut textfile = TextFile::new(file.path(), None).expect("file must load");
        assert!(textfile.load(0, 0).is_ok());
        let text = textfile.get(0, 0).expect("text should exist");
        assert_eq!(text, EXAMPLE_ASCII_TEXT);
    }

    #[test]
    pub fn test002_load_unicode() {
        let file = setup_unicode();
        let mut textfile = TextFile::new(file.path(), None).expect("file must load");
        let text = textfile.get_or_load(0, 0).expect("text should exist");
        assert_eq!(text, EXAMPLE_UNICODE_TEXT);
    }

    #[test]
    pub fn test003_subpart_of_loaded_frame() {
        let file = setup_ascii();
        let mut textfile = TextFile::new(file.path(), None).expect("file must load");
        assert!(textfile.load(0, 0).is_ok());
        let text = textfile.get(1, 10).expect("text should exist");
        assert_eq!(text, "Article 1");
    }

    #[test]
    pub fn test004_excerpt_in_frame() {
        let file = setup_ascii();
        let mut textfile = TextFile::new(file.path(), None).expect("file must load");
        let text = textfile.get_or_load(1, 10).expect("text should exist");
        assert_eq!(text, "Article 1");
    }

    #[test]
    pub fn test005_out_of_bounds() {
        let file = setup_ascii();
        let mut textfile = TextFile::new(file.path(), None).expect("file must load");
        assert!(textfile.load(0, 0).is_ok());
        assert!(textfile.get(1, 999).is_err());
    }
}
