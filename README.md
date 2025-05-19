[![Crate](https://img.shields.io/crates/v/textframe.svg)](https://crates.io/crates/textframe)
[![Docs](https://docs.rs/textframe/badge.svg)](https://docs.rs/textframe/)
[![GitHub build](https://github.com/proycon/textframe/actions/workflows/textframe.yml/badge.svg?branch=master)](https://github.com/proycon/textframe/actions/)
[![GitHub release](https://img.shields.io/github/release/proycon/textframe.svg)](https://GitHub.com/proycon/textframe/releases/)
[![Project Status: Active – The project has reached a stable, usable state and is being actively developed.](https://www.repostatus.org/badges/latest/active.svg)](https://www.repostatus.org/#active)

# TextFrame

TextFrame is a low-level Rust library to access plain text files, including plain-text corpora of considerable size.
Texts do not have to be accessed and loaded into memory in their entirety, but arbitrary sub-parts are loaded on-demand.
Requests are formulated with offsets in unicode character offsets or optionally also in lines (0-indexed).

## Features

This library takes care of mapping these to byte offsets (UTF-8) and loading the corresponding excerpt of the file from disk into memory. We call such an excerpt a *text frame*. Multiple discontinuous or partially overlapping text-frames might be loaded. Frames are only loaded from disk if no already loaded frame covers the offsets.

Negative values in offsets are supported and are interpreted as relative to the end of the document. This also applies to 0 as an end offset. All end offsets are non-inclusive. An offset of `(0,0)` by definition covers the entire text document.

* This library considers text as an immutable resource, text files on disk *MUST NOT* be modified after a `textframe::TextFile` object is associated with them.
* The mutability of `textframe::TextFile` itself only refers to the fact whether it is allowed to load further fragments from disk or not.
* When loading a text file, the entire text file is read in a streaming manner at first and an index is computed from unicode character positions to byte positions. This index can be written to a (binary) file which acts as a cache, preventing the need to recompute this index next time, and gaining a performance benefit.
* Existing frames are never unloaded or invalidated. Any text references (`&str`) therefore share the lifetime of the `textframe::TextFile` object. Depending on the order of requests, it does mean the loaded frames may have some overlap and be sub-optimal.
* Line ranges (0-indexed) are also supported.

## Installation

Add it to your Rust project as follows:

``cargo add textframe``

## Usage

Example:

```rust
use textframe::{TextFile,TextFileMode};

let mut textfile = TextFile::new("/tmp/test.txt", None, TextFileMode::WithLineIndex).expect("file must load");
//gets the text from 10 to 20 (unicode points), requires a mutable instance
let text: &str = textfile.get_or_load(10,20);

//or get the last 10 unicode points:
let text: &str = textfile.get_or_load(-10,0);

//once a frame is already loaded, you can use this instead, also works on an immutable instance:
let text: &str = textfile.get(10,20);

//you can get text per line-range as well, this will get the first line (0-indexed!)
let text: &str = textfile.get_or_load_lines(0,1);

//or the last line:
let text: &str = textfile.get_or_load_lines(-1,0);
```


## Related projects

* [textsurf](https://github.com/knaw-huc/textsurf) - A WebAPI around textframe. Serves text files over the web.

## Licence

GNU General Public Licence v3 only
