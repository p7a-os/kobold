//! Thin seam over the JSON backend.
//!
//! Everything goes through here so swapping sonic-rs for serde_json is a
//! one-file diff rather than a scatter of call sites.

pub use sonic_rs::Error;

#[inline]
pub fn from_slice<'a, T: serde::Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, Error> {
    sonic_rs::from_slice(bytes)
}

#[inline]
pub fn to_string<T: serde::Serialize + ?Sized>(value: &T) -> Result<String, Error> {
    sonic_rs::to_string(value)
}
