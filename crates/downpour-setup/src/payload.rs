//! Program files embedded at build time (see build.rs).

/// One file to install, at `path` relative to the install folder.
pub struct PayloadFile {
    pub path: &'static str,
    pub bytes: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/payload.rs"));
