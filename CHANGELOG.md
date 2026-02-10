
# v0.4.0 - 2026-01-05

[Miel Peeters]
* Added public methods for byte-based indexing
* Added public methods for conversion of (relative) line-ranges to (absolute) char-ranges (https://github.com/proycon/textframe/pull/3l)
* Fix: handle negative line numbers before boundary check ([#2](https://github.com/proycon/textframe/pull/2))
* Some refactoring

[Maarten van Gompel]
* Fixed some edge-cases for the new bytes_to_chars() and made get_byterange() and bytes_to_chars() safe

# v0.3.1 - 2025-11-10

* Added an extra error when a text is completely empty

# v0.3.0 - 2025-05-20

* Implemented line support (can be disabled by setting `--no-lines`)

# v0.2.0 - 2025-05-14

* Use smaller data structures for the position index ([#1](https://github.com/proycon/textframe/issues/1))
* implemented SHA-256 checksum
* added function get modification time from filesystem metadata

# v0.1.0 - 2025-01-15

Initial release, consider this a pre-release that needs more testing
