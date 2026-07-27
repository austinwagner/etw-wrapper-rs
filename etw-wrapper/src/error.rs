//! The error type returned by provider registration and event writing.

use std::fmt;

use crate::bindings::{ERROR_ARITHMETIC_OVERFLOW, ERROR_INVALID_DATA};

/// An error reported while registering a provider or writing an event.
///
/// Every variant maps to a Win32 error code through [`Error::win32_code`], so an error can still
/// be reported through interfaces that expect one.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Error {
    /// A derived length does not fit in the integer type of the field that carries it.
    LengthOverflow,
    /// An encoded value's length does not match the length declared by another field.
    LengthMismatch,
    /// Array elements sharing a single length field have differing lengths.
    MismatchedArrayLengths,
    /// A string field is missing its NUL terminator.
    MissingNulTerminator,
    /// A fixed-length string field has no room for a NUL terminator.
    EmptyFixedLengthString,
    /// A Windows API call failed with this Win32 error code.
    Windows(u32),
}

impl Error {
    /// Returns the Win32 error code that corresponds to this error.
    ///
    /// [`Error::LengthOverflow`] maps to `ERROR_ARITHMETIC_OVERFLOW`; the other input validation
    /// failures map to `ERROR_INVALID_DATA`.
    #[must_use]
    pub const fn win32_code(&self) -> u32 {
        match self {
            Error::LengthOverflow => ERROR_ARITHMETIC_OVERFLOW,
            Error::LengthMismatch
            | Error::MismatchedArrayLengths
            | Error::MissingNulTerminator
            | Error::EmptyFixedLengthString => ERROR_INVALID_DATA,
            Error::Windows(code) => *code,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::LengthOverflow => f.write_str("length does not fit in the declared length type"),
            Error::LengthMismatch => {
                f.write_str("value length does not match the declared length field")
            }
            Error::MismatchedArrayLengths => {
                f.write_str("array elements sharing a length field have differing lengths")
            }
            Error::MissingNulTerminator => f.write_str("string is not NUL terminated"),
            Error::EmptyFixedLengthString => {
                f.write_str("fixed-length string has no room for a NUL terminator")
            }
            // Defers to the OS for a localized message.
            Error::Windows(code) => {
                fmt::Display::fmt(&std::io::Error::from_raw_os_error(*code as i32), f)
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<Error> for std::io::Error {
    fn from(error: Error) -> Self {
        std::io::Error::from_raw_os_error(error.win32_code() as i32)
    }
}

/// A specialized [`Result`](std::result::Result) for this crate's fallible operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Converts a Win32 status code returned by an ETW API into a [`Result`].
pub(crate) fn win32_result(code: u32) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(Error::Windows(code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win32_result_maps_zero_to_success() {
        assert_eq!(win32_result(0), Ok(()));
        assert_eq!(win32_result(5), Err(Error::Windows(5)));
    }

    #[test]
    fn io_error_conversion_preserves_the_win32_code() {
        // ERROR_ACCESS_DENIED
        let io: std::io::Error = Error::Windows(5).into();
        assert_eq!(io.raw_os_error(), Some(5));

        let io: std::io::Error = Error::LengthOverflow.into();
        assert_eq!(io.raw_os_error(), Some(ERROR_ARITHMETIC_OVERFLOW as i32));
    }

    #[test]
    fn windows_errors_display_the_os_message() {
        let message = Error::Windows(5).to_string();
        assert!(!message.is_empty());
        assert!(!message.contains("Windows"));
    }
}
