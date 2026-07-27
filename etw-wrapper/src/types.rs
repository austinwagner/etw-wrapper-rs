//! Extensions for the generated Windows ABI types exposed by this crate.

use core::hash::{Hash, Hasher};

pub use crate::bindings::{EVENT_DATA_DESCRIPTOR, EVENT_DESCRIPTOR, FILETIME, GUID, SYSTEMTIME};

impl Default for GUID {
    fn default() -> Self {
        Self::from_u128(0)
    }
}

impl PartialEq for GUID {
    fn eq(&self, other: &Self) -> bool {
        self.data1 == other.data1
            && self.data2 == other.data2
            && self.data3 == other.data3
            && self.data4 == other.data4
    }
}

impl Eq for GUID {}

impl Hash for GUID {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.data1.hash(state);
        self.data2.hash(state);
        self.data3.hash(state);
        self.data4.hash(state);
    }
}

impl GUID {
    /// Converts a `GUID` back to its canonical `u128` value.
    #[must_use]
    pub const fn to_u128(&self) -> u128 {
        ((self.data1 as u128) << 96)
            + ((self.data2 as u128) << 80)
            + ((self.data3 as u128) << 64)
            + u64::from_be_bytes(self.data4) as u128
    }

    /// Creates a `GUID` from its individual fields.
    #[must_use]
    pub const fn from_values(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Self {
        Self {
            data1,
            data2,
            data3,
            data4,
        }
    }
}

impl core::fmt::Debug for GUID {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:08X?}-{:04X?}-{:04X?}-{:02X?}{:02X?}-{:02X?}{:02X?}{:02X?}{:02X?}{:02X?}{:02X?}",
            self.data1,
            self.data2,
            self.data3,
            self.data4[0],
            self.data4[1],
            self.data4[2],
            self.data4[3],
            self.data4[4],
            self.data4[5],
            self.data4[6],
            self.data4[7]
        )
    }
}

impl From<u128> for GUID {
    fn from(value: u128) -> Self {
        Self::from_u128(value)
    }
}

impl From<GUID> for u128 {
    fn from(value: GUID) -> Self {
        value.to_u128()
    }
}

macro_rules! impl_value_traits {
    ($type:ty { $($field:ident),+ $(,)? }) => {
        impl core::fmt::Debug for $type {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                let mut value = f.debug_struct(stringify!($type));
                $(value.field(stringify!($field), &self.$field);)+
                value.finish()
            }
        }

        impl PartialEq for $type {
            fn eq(&self, other: &Self) -> bool {
                true $(&& self.$field == other.$field)+
            }
        }

        impl Eq for $type {}

        impl Hash for $type {
            fn hash<H: Hasher>(&self, state: &mut H) {
                $(self.$field.hash(state);)+
            }
        }
    };
}

impl_value_traits!(FILETIME {
    dwLowDateTime,
    dwHighDateTime,
});
impl_value_traits!(SYSTEMTIME {
    wYear,
    wMonth,
    wDayOfWeek,
    wDay,
    wHour,
    wMinute,
    wSecond,
    wMilliseconds,
});
impl_value_traits!(EVENT_DESCRIPTOR {
    Id,
    Version,
    Channel,
    Level,
    Opcode,
    Task,
    Keyword,
});

// `windows-bindgen` cannot derive these through the descriptor's trailing union. Comparing and
// hashing the union as its `Reserved` word covers every possible bit pattern and matches its ABI.
impl core::fmt::Debug for EVENT_DATA_DESCRIPTOR {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EVENT_DATA_DESCRIPTOR")
            .field("Ptr", &self.Ptr)
            .field("Size", &self.Size)
            .field("Reserved", unsafe { &self.Anonymous.Reserved })
            .finish()
    }
}

impl PartialEq for EVENT_DATA_DESCRIPTOR {
    fn eq(&self, other: &Self) -> bool {
        self.Ptr == other.Ptr
            && self.Size == other.Size
            && unsafe { self.Anonymous.Reserved == other.Anonymous.Reserved }
    }
}

impl Eq for EVENT_DATA_DESCRIPTOR {}

impl Hash for EVENT_DATA_DESCRIPTOR {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.Ptr.hash(state);
        self.Size.hash(state);
        unsafe { self.Anonymous.Reserved.hash(state) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 6ba7b810-9dad-11d1-80b4-00c04fd430c8, the RFC 4122 DNS namespace UUID. Every field differs,
    // so a byte-order mistake in `from_u128` cannot go unnoticed.
    const SAMPLE: u128 = 0x6ba7b810_9dad_11d1_80b4_00c04fd430c8;

    #[test]
    fn guid_round_trips_through_u128() {
        for value in [0, u128::MAX, SAMPLE, 1, 1 << 64] {
            assert_eq!(GUID::from_u128(value).to_u128(), value);
        }
    }

    #[test]
    fn guid_debug_uses_the_canonical_text_form() {
        assert_eq!(
            format!("{:?}", GUID::from_u128(SAMPLE)),
            "6BA7B810-9DAD-11D1-80B4-00C04FD430C8"
        );
    }

    #[test]
    fn guid_from_values_matches_from_u128() {
        let expected = GUID::from_u128(SAMPLE);
        let actual = GUID::from_values(
            0x6ba7b810,
            0x9dad,
            0x11d1,
            [0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8],
        );
        assert_eq!(actual, expected);
    }

    #[test]
    fn event_data_descriptor_default_selects_the_plain_payload_type() {
        let descriptor = EVENT_DATA_DESCRIPTOR::default();
        assert_eq!(
            unsafe { descriptor.Anonymous.Reserved },
            crate::bindings::EVENT_DATA_DESCRIPTOR_TYPE_NONE
        );
    }
}
