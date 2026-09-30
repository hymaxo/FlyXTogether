//! String helpers for crossing the C boundary. Pure, so they are unit-tested.

use std::ffi::{CString, c_char};

/// Converts a Rust string to a C string, dropping any interior NUL bytes
/// instead of failing.
pub fn to_cstring(s: &str) -> CString {
    let bytes: Vec<u8> = s.bytes().filter(|&b| b != 0).collect();
    CString::new(bytes).expect("NUL bytes were removed")
}

/// Reads a NUL-terminated string out of a buffer filled by C code. Invalid
/// UTF-8 is replaced; a buffer without a NUL is read in full.
pub fn c_buf_to_string(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Copies `s` into a C output buffer of `capacity` bytes, truncating on a
/// UTF-8 character boundary so that the result always ends with a NUL.
///
/// # Safety
/// `dst` must be valid for writes of `capacity` bytes.
pub unsafe fn write_c_buf(dst: *mut c_char, capacity: usize, s: &str) {
    if dst.is_null() || capacity == 0 {
        return;
    }
    let bytes = truncate_utf8(s, capacity - 1).as_bytes();
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len());
        *dst.add(bytes.len()) = 0;
    }
}

/// Longest prefix of `s` that fits in `max_bytes` without splitting a character.
pub fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_cstring_strips_interior_nul() {
        assert_eq!(to_cstring("a\0b").as_bytes(), b"ab");
        assert_eq!(to_cstring("").as_bytes(), b"");
    }

    #[test]
    fn c_buf_stops_at_first_nul() {
        assert_eq!(
            c_buf_to_string(b"Cessna_172SP.acf\0garbage"),
            "Cessna_172SP.acf"
        );
        assert_eq!(c_buf_to_string(b"no terminator"), "no terminator");
        assert_eq!(c_buf_to_string(b"\0"), "");
    }

    #[test]
    fn c_buf_replaces_invalid_utf8() {
        assert_eq!(c_buf_to_string(&[b'a', 0xff, b'b', 0]), "a\u{fffd}b");
    }

    #[test]
    fn write_c_buf_truncates_and_terminates() {
        let mut buf = [0x55 as c_char; 6];
        unsafe { write_c_buf(buf.as_mut_ptr(), buf.len(), "FlyXTogether") };
        let bytes: Vec<u8> = buf.iter().map(|&c| c as u8).collect();
        assert_eq!(&bytes, b"FlyXT\0");
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        // "é" is two bytes; cutting after one byte must back off.
        assert_eq!(truncate_utf8("aé", 2), "a");
        assert_eq!(truncate_utf8("aé", 3), "aé");
        assert_eq!(truncate_utf8("abc", 0), "");
    }
}
