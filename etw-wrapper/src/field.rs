//! Serialization primitives that turn typed values into [`EVENT_DATA_DESCRIPTOR`] instances.

use crate::EVENT_DATA_DESCRIPTOR;
use std::marker;

pub use safe_sid::{Sid, SidBuf};

#[repr(transparent)]
#[derive(Default)]
pub struct EventDataDescriptor<'a> {
    inner: EVENT_DATA_DESCRIPTOR,
    _marker: marker::PhantomData<&'a ()>,
}

impl<'a> EventDataDescriptor<'a> {
    /// Creates a descriptor over a raw memory region.
    ///
    /// # Safety
    ///
    /// If `size` is nonzero, `ptr` must point to a readable memory region of at least `size`
    /// bytes. That region must remain allocated and readable for the entire lifetime `'a`, and it
    /// must not be mutated in a way that races with an event write using this descriptor.
    pub unsafe fn new(ptr: u64, size: u32) -> Self {
        Self {
            inner: EVENT_DATA_DESCRIPTOR {
                Ptr: ptr,
                Size: size,
                ..Default::default()
            },
            _marker: marker::PhantomData,
        }
    }
}

/// Creates a descriptor over a `Copy` scalar.
#[inline]
pub fn scalar<T: Copy>(v: &T) -> EventDataDescriptor<'_> {
    // SAFETY: the descriptor borrows `v`, which remains readable for the returned lifetime.
    unsafe { EventDataDescriptor::new(v as *const T as u64, size_of::<T>() as u32) }
}

/// Creates a descriptor over a contiguous slice of `Copy` values.
#[inline]
pub fn slice<T: Copy>(values: &[T]) -> EventDataDescriptor<'_> {
    // SAFETY: the descriptor borrows `values`, which remains readable for the returned lifetime.
    unsafe { EventDataDescriptor::new(values.as_ptr() as u64, size_of_val(values) as u32) }
}

/// Creates a descriptor over an ANSI string (win:AnsiString).
///
/// # Panics
///
/// Panics if the buffer is not NUL-terminated.
#[inline]
pub fn str8(buf: &[u8]) -> EventDataDescriptor<'_> {
    assert_eq!(buf.last(), Some(&0));
    bytes(buf)
}

/// Creates a descriptor over a byte slice (win:Binary).
#[inline]
pub fn bytes(b: &[u8]) -> EventDataDescriptor<'_> {
    // SAFETY: the descriptor borrows `b`, which remains readable for the returned lifetime.
    unsafe { EventDataDescriptor::new(b.as_ptr() as u64, b.len() as u32) }
}

/// Converts a slice length into the type of another field.
///
/// Generated code uses this function for `win:Binary length="OtherField"`. The length field is
/// derived from the blob's length and emitted in place without being exposed as a parameter.
/// Returns `ERROR_ARITHMETIC_OVERFLOW` if the length does not fit in `T`.
#[inline]
pub fn checked_len<T: TryFrom<usize>>(len: usize) -> crate::Result<T> {
    T::try_from(len).map_err(|_| {
        windows::Win32::Foundation::ERROR_ARITHMETIC_OVERFLOW
            .to_hresult()
            .into()
    })
}

/// Returns the common length of a collection of byte slices.
///
/// Generated array wrappers use this for `win:Binary` fields that combine `count="..."` with a
/// referenced `length="..."`. An empty collection has an element length of zero.
#[doc(hidden)]
#[inline]
pub fn uniform_len<T: AsRef<[u8]>>(values: &[T]) -> crate::Result<usize> {
    let Some(first) = values.first() else {
        return Ok(0);
    };
    let len = first.as_ref().len();
    if values.iter().all(|value| value.as_ref().len() == len) {
        Ok(len)
    } else {
        Err(windows::Win32::Foundation::ERROR_INVALID_DATA
            .to_hresult()
            .into())
    }
}

/// Validates that an encoded value has the length declared by another manifest field.
#[doc(hidden)]
#[inline]
pub fn ensure_len(actual: usize, expected: usize) -> crate::Result<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(windows::Win32::Foundation::ERROR_INVALID_DATA
            .to_hresult()
            .into())
    }
}

/// Creates a descriptor over a UTF-16 buffer (win:UnicodeString).
///
/// # Panics
///
/// Panics if the buffer is not NUL-terminated.
#[inline]
pub fn str16(buf: &[u16]) -> EventDataDescriptor<'_> {
    assert_eq!(buf.last(), Some(&0));

    // SAFETY: the descriptor borrows `buf`, which remains readable for the returned lifetime.
    unsafe { EventDataDescriptor::new(buf.as_ptr() as u64, size_of_val(buf) as u32) }
}

/// Encodes `s` as NUL-terminated UTF-16.
///
/// Interior NUL values are replaced with spaces.
#[inline]
pub fn to_u16cstring(s: &str) -> Vec<u16> {
    let mut buf = Vec::with_capacity(s.len() + 1);
    let mut units = [0u16; 2];

    for ch in s.chars() {
        let ch = if ch == '\0' { ' ' } else { ch };
        buf.extend_from_slice(ch.encode_utf16(&mut units));
    }

    buf.push(0);
    buf
}

/// Encodes `s` as a fixed-length, NUL-terminated UTF-16 string for an ETW
/// manifest field with `length="len"`.
///
/// The declared length includes the terminator. Content is therefore limited to
/// `len - 1` code units, without splitting a surrogate pair. Short content
/// is padded with spaces before the terminator so the returned buffer always
/// contains exactly `len` code units.
///
/// Interior NUL values are replaced with spaces.
///
/// # Panics
///
/// Panics if `len` is zero because a fixed-length ETW string must include a
/// NUL terminator.
#[inline]
pub fn to_u16cstring_fixed_len(s: &str, len: usize) -> Vec<u16> {
    assert!(len > 0, "len must include space for the NUL terminator");

    let content_limit = len - 1;
    let mut buf = Vec::with_capacity(len);
    let mut units = [0u16; 2];

    for ch in s.chars() {
        let ch = if ch == '\0' { ' ' } else { ch };
        let width = ch.len_utf16();

        if buf.len() + width > content_limit {
            break;
        }

        buf.extend_from_slice(ch.encode_utf16(&mut units));
    }

    buf.resize(content_limit, b' ' as u16);
    buf.push(0);
    buf
}

/// Encodes `s` as NUL-terminated UTF-8 for a `win:AnsiString` field whose output type explicitly
/// selects UTF-8, JSON, or XML.
///
/// Interior NUL values are replaced with spaces.
#[inline]
pub fn to_cstring(s: &str) -> Vec<u8> {
    s.bytes()
        .map(|byte| if byte == 0 { b' ' } else { byte })
        .chain(std::iter::once(0))
        .collect()
}

/// Encodes `s` as a fixed-length, NUL-terminated UTF-8 string for an ETW `win:AnsiString` field
/// whose output type explicitly selects UTF-8, JSON, or XML and whose manifest declares
/// `length="len"`.
///
/// The declared length includes the terminator. UTF-8 content is therefore
/// limited to `len - 1` bytes, without splitting an encoded character. Short
/// content is padded with spaces before the terminator so the returned buffer
/// always contains exactly `len` bytes.
///
/// Interior NUL values are replaced with spaces.
///
/// # Panics
///
/// Panics if `len` is zero because a fixed-length ETW string must include a NUL
/// terminator.
#[inline]
pub fn to_cstring_fixed_len(s: &str, len: usize) -> Vec<u8> {
    assert!(len > 0, "len must include space for the NUL terminator");

    let content_limit = len - 1;
    let mut buf = Vec::with_capacity(len);
    let mut bytes = [0u8; 4];

    for ch in s.chars() {
        let ch = if ch == '\0' { ' ' } else { ch };
        let width = ch.len_utf8();
        if buf.len() + width > content_limit {
            break;
        }

        buf.extend_from_slice(ch.encode_utf8(&mut bytes).as_bytes());
    }

    buf.resize(content_limit, b' ');
    buf.push(0);
    buf
}

/// Creates a descriptor over a [`Sid`].
#[inline]
pub fn sid(sid: &Sid) -> EventDataDescriptor<'_> {
    let bytes = sid.as_bytes();
    // SAFETY: the descriptor borrows `sid` through `bytes`, which remains readable for the
    // returned lifetime.
    unsafe { EventDataDescriptor::new(bytes.as_ptr() as u64, bytes.len() as u32) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_u16cstring_replaces_interior_nul_with_space() {
        assert_eq!(
            to_u16cstring("a\0b"),
            vec![b'a' as u16, b' ' as u16, b'b' as u16, 0]
        );
        assert_eq!(to_u16cstring("hi"), vec![b'h' as u16, b'i' as u16, 0]);
    }

    #[test]
    fn to_u16cstring_fixed_len_matches_etw_layout() {
        assert_eq!(
            to_u16cstring_fixed_len("hello", 3),
            vec![b'h' as u16, b'e' as u16, 0]
        );
        assert_eq!(
            to_u16cstring_fixed_len("hello", 10),
            vec![
                b'h' as u16,
                b'e' as u16,
                b'l' as u16,
                b'l' as u16,
                b'o' as u16,
                b' ' as u16,
                b' ' as u16,
                b' ' as u16,
                b' ' as u16,
                0,
            ]
        );
        assert_eq!(
            to_u16cstring_fixed_len("a\u{1F600}b", 3),
            vec![b'a' as u16, b' ' as u16, 0]
        );
        assert_eq!(
            to_u16cstring_fixed_len("a\u{1F600}b", 4),
            vec![b'a' as u16, 0xD83D, 0xDE00, 0]
        );
        assert_eq!(
            to_u16cstring_fixed_len("a\0b", 3),
            vec![b'a' as u16, b' ' as u16, 0]
        );
        assert_eq!(to_u16cstring_fixed_len("ignored", 1), vec![0]);
    }

    #[test]
    #[should_panic(expected = "len must include space for the NUL terminator")]
    fn to_u16cstring_fixed_len_rejects_zero_length() {
        to_u16cstring_fixed_len("", 0);
    }

    #[test]
    fn to_cstring_replaces_interior_nul_with_space() {
        assert_eq!(to_cstring("a\0b"), b"a b\0");
        assert_eq!(to_cstring("hi"), b"hi\0");
    }

    #[test]
    fn to_cstring_fixed_len_matches_etw_layout() {
        assert_eq!(to_cstring_fixed_len("hello", 3), b"he\0");
        assert_eq!(to_cstring_fixed_len("hello", 10), b"hello    \0");
        assert_eq!(to_cstring_fixed_len("a\u{00E9}b", 3), b"a \0");
        assert_eq!(to_cstring_fixed_len("a\u{00E9}b", 4), b"a\xC3\xA9\0");
        assert_eq!(to_cstring_fixed_len("a\0b", 3), b"a \0");
        assert_eq!(to_cstring_fixed_len("ignored", 1), b"\0");
    }

    #[test]
    #[should_panic(expected = "len must include space for the NUL terminator")]
    fn to_cstring_fixed_len_rejects_zero_length() {
        to_cstring_fixed_len("", 0);
    }

    #[test]
    fn slice_uses_the_full_contiguous_size() {
        let values = [1u32, 2, 3];
        let descriptor = slice(&values);
        assert_eq!(descriptor.inner.Size, 3 * size_of::<u32>() as u32);
        assert_eq!(descriptor.inner.Ptr, values.as_ptr() as u64);
    }

    #[test]
    fn uniform_len_accepts_empty_and_equal_slices() {
        let empty: [&[u8]; 0] = [];
        assert_eq!(uniform_len(&empty).unwrap(), 0);
        assert_eq!(
            uniform_len(&[b"abc".as_slice(), b"def".as_slice()]).unwrap(),
            3
        );
    }

    #[test]
    fn uniform_len_rejects_mixed_lengths() {
        assert!(uniform_len(&[b"a".as_slice(), b"bc".as_slice()]).is_err());
    }

    #[test]
    fn ensure_len_rejects_mismatches() {
        assert!(ensure_len(3, 3).is_ok());
        assert!(ensure_len(2, 3).is_err());
    }
}
