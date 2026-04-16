//! Album forwarding shims have been removed.
//!
//! All album persistence now lives in `AlbumRepository`
//! (`library/album/repository.rs`). This module is kept only so that
//! `mod albums;` in `db/mod.rs` still compiles.
