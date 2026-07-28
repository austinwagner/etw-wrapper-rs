//! Crate-owned Windows ABI types.

use core::hash::{Hash, Hasher};

pub(crate) use crate::bindings::EVENT_DATA_DESCRIPTOR;

/// Describes an ETW event's identity and filtering metadata.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct EventDescriptor {
    /// The event identifier.
    pub id: u16,
    /// The event definition version.
    pub version: u8,
    /// The event channel.
    pub channel: u8,
    /// The event severity level.
    pub level: u8,
    /// The operation performed by the event.
    pub opcode: u8,
    /// The event task.
    pub task: u16,
    /// The event category bitmask.
    pub keyword: u64,
}

/// A Windows file time represented as 100-nanosecond intervals since January 1, 1601 UTC.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FileTime {
    /// The low-order 32 bits of the file time.
    pub low_date_time: u32,
    /// The high-order 32 bits of the file time.
    pub high_date_time: u32,
}

/// A Windows globally unique identifier.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Guid {
    /// The first 32 bits of the GUID.
    pub data1: u32,
    /// The next 16 bits of the GUID.
    pub data2: u16,
    /// The next 16 bits of the GUID.
    pub data3: u16,
    /// The final 64 bits of the GUID.
    pub data4: [u8; 8],
}

/// A Windows date and time with millisecond precision.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SystemTime {
    /// The year.
    pub year: u16,
    /// The month.
    pub month: u16,
    /// The day of the week.
    pub day_of_week: u16,
    /// The day of the month.
    pub day: u16,
    /// The hour.
    pub hour: u16,
    /// The minute.
    pub minute: u16,
    /// The second.
    pub second: u16,
    /// The millisecond.
    pub milliseconds: u16,
}

impl Default for Guid {
    fn default() -> Self {
        Self::from_u128(0)
    }
}

impl PartialEq for Guid {
    fn eq(&self, other: &Self) -> bool {
        self.data1 == other.data1
            && self.data2 == other.data2
            && self.data3 == other.data3
            && self.data4 == other.data4
    }
}

impl Eq for Guid {}

impl Hash for Guid {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.data1.hash(state);
        self.data2.hash(state);
        self.data3.hash(state);
        self.data4.hash(state);
    }
}

impl Guid {
    /// Creates a `Guid` from its canonical `u128` value.
    #[must_use]
    pub const fn from_u128(uuid: u128) -> Self {
        Self {
            data1: (uuid >> 96) as u32,
            data2: (uuid >> 80 & 0xffff) as u16,
            data3: (uuid >> 64 & 0xffff) as u16,
            data4: (uuid as u64).to_be_bytes(),
        }
    }

    /// Converts a `Guid` back to its canonical `u128` value.
    #[must_use]
    pub const fn to_u128(&self) -> u128 {
        ((self.data1 as u128) << 96)
            + ((self.data2 as u128) << 80)
            + ((self.data3 as u128) << 64)
            + u64::from_be_bytes(self.data4) as u128
    }

    /// Creates a `Guid` from its individual fields.
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

impl core::fmt::Debug for Guid {
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

impl From<u128> for Guid {
    fn from(value: u128) -> Self {
        Self::from_u128(value)
    }
}

impl From<Guid> for u128 {
    fn from(value: Guid) -> Self {
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

impl_value_traits!(FileTime {
    low_date_time,
    high_date_time,
});
impl_value_traits!(SystemTime {
    year,
    month,
    day_of_week,
    day,
    hour,
    minute,
    second,
    milliseconds,
});
impl_value_traits!(EventDescriptor {
    id,
    version,
    channel,
    level,
    opcode,
    task,
    keyword,
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
    use std::mem::offset_of;

    // 6ba7b810-9dad-11d1-80b4-00c04fd430c8, the RFC 4122 DNS namespace UUID. Every field differs,
    // so a byte-order mistake in `from_u128` cannot go unnoticed.
    const SAMPLE: u128 = 0x6ba7b810_9dad_11d1_80b4_00c04fd430c8;

    #[test]
    fn public_types_preserve_the_win32_abi_layout() {
        assert_eq!(size_of::<Guid>(), size_of::<crate::bindings::GUID>());
        assert_eq!(align_of::<Guid>(), align_of::<crate::bindings::GUID>());
        assert_eq!(offset_of!(Guid, data1), 0);
        assert_eq!(offset_of!(Guid, data2), 4);
        assert_eq!(offset_of!(Guid, data3), 6);
        assert_eq!(offset_of!(Guid, data4), 8);

        assert_eq!(size_of::<EventDescriptor>(), 16);
        assert_eq!(align_of::<EventDescriptor>(), 8);
        assert_eq!(offset_of!(EventDescriptor, id), 0);
        assert_eq!(offset_of!(EventDescriptor, version), 2);
        assert_eq!(offset_of!(EventDescriptor, channel), 3);
        assert_eq!(offset_of!(EventDescriptor, level), 4);
        assert_eq!(offset_of!(EventDescriptor, opcode), 5);
        assert_eq!(offset_of!(EventDescriptor, task), 6);
        assert_eq!(offset_of!(EventDescriptor, keyword), 8);

        assert_eq!(size_of::<FileTime>(), 8);
        assert_eq!(align_of::<FileTime>(), 4);
        assert_eq!(offset_of!(FileTime, low_date_time), 0);
        assert_eq!(offset_of!(FileTime, high_date_time), 4);

        assert_eq!(size_of::<SystemTime>(), 16);
        assert_eq!(align_of::<SystemTime>(), 2);
        assert_eq!(offset_of!(SystemTime, year), 0);
        assert_eq!(offset_of!(SystemTime, month), 2);
        assert_eq!(offset_of!(SystemTime, day_of_week), 4);
        assert_eq!(offset_of!(SystemTime, day), 6);
        assert_eq!(offset_of!(SystemTime, hour), 8);
        assert_eq!(offset_of!(SystemTime, minute), 10);
        assert_eq!(offset_of!(SystemTime, second), 12);
        assert_eq!(offset_of!(SystemTime, milliseconds), 14);
    }

    #[test]
    fn guid_round_trips_through_u128() {
        for value in [0, u128::MAX, SAMPLE, 1, 1 << 64] {
            assert_eq!(Guid::from_u128(value).to_u128(), value);
        }
    }

    #[test]
    fn guid_debug_uses_the_canonical_text_form() {
        assert_eq!(
            format!("{:?}", Guid::from_u128(SAMPLE)),
            "6BA7B810-9DAD-11D1-80B4-00C04FD430C8"
        );
    }

    #[test]
    fn guid_from_values_matches_from_u128() {
        let expected = Guid::from_u128(SAMPLE);
        let actual = Guid::from_values(
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
