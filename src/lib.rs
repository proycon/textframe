use smallvec::{smallvec, SmallVec};
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::string::FromUtf8Error;

type FrameHandle = u32;

#[derive(Debug, Clone)]
pub struct PositionData {
    /// Unicode point
    charpos: usize,
    /// UTF-8 byte offset
    bytepos: usize,
    /// Line
    line: Option<usize>,
    /// Size in bytes of this data point and all data points until the next one in the index
    size: u8,
}

#[derive(Debug)]
pub enum Error {
    OutOfBoundsError { begin: isize, end: isize },
    IOError(std::io::Error),
    Utf8Error(FromUtf8Error),
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
        }
    }
}

impl std::error::Error for Error {}

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

pub struct TextFile {
    /// The path to the text file
    path: PathBuf,

    /// The path to the index file
    indexpath: PathBuf,

    /// Holds loaded excerpts of the text, in order
    frames: BTreeMap<usize, SmallVec<[TextFrame; 1]>>,

    positionindex: PositionIndex,
}

#[derive(Debug, Default, Clone)]
struct PositionIndex {
    /// Length of the text file in characters
    charsize: usize,

    /// Size of the text file in bytes
    bytesize: usize,

    /// Maps character positions to bytes
    positions: Vec<PositionData>,
}

impl TextFile {
    /// Associates with an existing text file on disk
    pub fn new(path: PathBuf, indexpath: PathBuf) -> Self {
        Self {
            path,
            frames: BTreeMap::new(),
            positionindex: PositionIndex::default(),
        }
    }

    /// Returns a text fragment. The text must already be in memory.
    pub fn text(&self, begin: isize, end: isize) -> Result<&str, Error> {
        let (beginchar, endchar) = self.absolute_pos(begin, end)?;
        let beginbyte = self.chars_to_bytes(beginchar)?;
        let endbyte = self.chars_to_bytes(endchar)?;
        if let Some(frame) = self.frame(beginbyte, endbyte) {
            Ok(&frame.text.as_str()[(beginbyte - frame.beginbyte)..(endbyte - frame.beginbyte)])
        } else {
            Err(Error::NotLoaded)
        }
    }

    pub fn load(&mut self, beginchar: usize, endchar: usize) -> Result<(), Error> {
        let beginbyte = self.chars_to_bytes(beginchar)?;
        let endbyte = self.chars_to_bytes(endchar)?;
        match self.load_frame(beginbyte, endbyte, beginchar, endchar) {
            Ok(frame) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn has_frame(&self, beginbyte: usize, endbyte: usize) -> bool {
        let mut iter = self.frames.range(0..=beginbyte);
        // read the (double-ended) iterator backwards
        // and see if we find a frame that holds the bytes we want
        while let Some((_, frames)) = iter.next_back() {
            for frame in frames {
                if frame.endbyte > endbyte {
                    return true;
                }
            }
        }
        false
    }

    /// Returns an existing frame that holds the given byte offset (if any is loaded)
    fn frame(&self, beginbyte: usize, endbyte: usize) -> Option<&TextFrame> {
        let mut iter = self.frames.range(0..=beginbyte);
        // read the (double-ended) iterator backwards
        // and see if we find a frame that holds the bytes we want
        while let Some((_, frames)) = iter.next_back() {
            for frame in frames {
                if frame.endbyte > endbyte {
                    return Some(frame);
                }
            }
        }
        None
    }

    fn load_frame(
        &mut self,
        beginbyte: usize,
        endbyte: usize,
        beginchar: usize,
        endchar: usize,
    ) -> Result<&TextFrame, Error> {
        let mut buffer: Vec<u8> = Vec::with_capacity(endbyte - beginbyte);
        let mut file = File::open(self.path.as_path()).map_err(|e| Error::IOError(e))?;
        file.seek(SeekFrom::Start(beginbyte as u64))
            .map_err(|e| Error::IOError(e))?;
        file.read_exact(&mut buffer)
            .map_err(|e| Error::IOError(e))?;
        let frame = TextFrame {
            beginbyte,
            endbyte,
            beginchar,
            endchar,
            text: String::from_utf8(buffer).map_err(|e| Error::Utf8Error(e))?,
        };
        match self.frames.entry(beginbyte) {
            Entry::Occupied(mut entry) => entry.get_mut().push(frame),
            Entry::Vacant(entry) => {
                entry.insert(smallvec!(frame));
            }
        }
        Ok(self.frames.get(&beginbyte).unwrap().last().unwrap())
    }

    /// Convert character offset to byte offsets
    pub fn chars_to_bytes(&self, charpos: usize) -> Result<usize, Error> {
        match self
            .positionindex
            .positions
            .binary_search_by_key(&charpos, |posdata: &PositionData| posdata.charpos)
        {
            Ok(index) => {
                //exact match
                let posdata = self.positionindex.positions.get(index).unwrap();
                Ok(posdata.bytepos())
            }
            Err(index) => {
                //miss, compute from the item just before
                let posdata = self.positionindex.positions.get(index).unwrap();
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

    /// Convert relative character offset to an absolute one
    pub fn absolute_pos(&self, begin: isize, end: isize) -> Result<(usize, usize), Error> {
        if begin >= 0 && end > 0 && end < begin {
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
            Err(Error::OutOfBoundsError { begin, end })
        }
    }

    fn create_index(&mut self) {}

    pub fn write_index(&self) {}

    pub fn load_index(&self) {}
}

/// A frame is a fragment of loaded text
pub struct TextFrame {
    beginbyte: usize,
    endbyte: usize,
    beginchar: usize,
    endchar: usize,
    text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        assert_eq!(result, 4);
    }
}
