//! Thin seam over the JSON backend.
//!
//! Mirrors `kobold::json` deliberately rather than sharing it: this crate
//! must not depend on Kobold, and the seam is six lines. Both exist so
//! swapping sonic-rs is a one-file diff on each side rather than a scatter of
//! call sites.

pub use sonic_rs::Error;

#[inline]
pub fn from_slice<'a, T: serde::Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, Error> {
    sonic_rs::from_slice(bytes)
}

#[inline]
pub fn to_string<T: serde::Serialize + ?Sized>(value: &T) -> Result<String, Error> {
    sonic_rs::to_string(value)
}
