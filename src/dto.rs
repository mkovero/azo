use std::ffi::{CString, c_long};
use std::num::NonZeroI32;
use crate::utils::cstring_from_bytes_until_nul;
use azo_sys as sys;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChannelCounts {
    pub in_: c_long,
    pub out: c_long
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Latencies {
    pub in_: c_long,
    pub out: c_long
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SamplePosition {
    pub position  : i64,
    pub time_stamp: i64
}

impl From<SamplePosition> for sys::Time {
    fn from(pos: SamplePosition) -> Self {
        Self {
            _reserved: [0; 4],
            time_info: sys::TimeInfo {
                speed          : Default::default(), // not marked valid
                system_time    : pos.time_stamp.into(),
                sample_position: pos.position  .into(),
                sample_rate    : Default::default(), // not marked valid
                flags          : sys::TimeInfoFlags::SYSTEM_TIME_VALID
                               | sys::TimeInfoFlags::SAMPLE_POSITION_VALID,
                _reserved      : [0; 12]
            },
            time_code: sys::TimeCode::invalid()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChannelInfoResponse {
    pub is_active  : bool,
	pub group      : sys::ChannelGroupIndex,
	pub sample_type: sys::SampleType,
	pub name       : CString
}

impl From<sys::ChannelInfo> for ChannelInfoResponse {
    fn from(value: sys::ChannelInfo) -> Self {
        Self {
            is_active  : value.is_active.try_into().unwrap_or(false),
            group      : value.channel_group,
            sample_type: value.sample_type,
            name       : cstring_from_bytes_until_nul(&value.name)
        }
    }
}

/// A clock source as the driver lists it, with its name cut at the first NUL: drivers
/// leave whatever was in their 32-byte buffer after the terminator.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClockSourceResponse {
	/// For use in [`Driver::set_clock_source()`](crate::driver::Driver::set_clock_source)
	pub index             : sys::ClockSourceIndex,
	/// E.g. S/PDIF, AES/EBU
	pub associated_channel: sys::ChannelIndex,
	pub associated_group  : sys::ChannelGroupIndex,
	/// The source the driver currently runs on.
	pub is_current        : bool,
	pub name              : CString
}

impl From<sys::ClockSource> for ClockSourceResponse {
	fn from(value: sys::ClockSource) -> Self {
		Self {
			index             : value.index,
			associated_channel: value.associated_channel,
			associated_group  : value.associated_group,
			is_current        : value.is_current_source.try_into().unwrap_or(false),
			name              : cstring_from_bytes_until_nul(&value.name)
		}
	}
}

/// Technically, [`Self::min`] and [`Self::max`] should be guarded by the same [`Option`] as [`Self::granularity`],
/// as the spec states that `min == max` requires `granularity == 0`.
/// Practically however:
/// 1. Even Steinberg's own "built-in" driver violates this requirement, which sets a problematic precedent
/// 2. The spec unfortunately fails to state the inverse, which technically allows `granularity == 0`
///    as an alternative expression of `granularity == max - min`, when `min != max`.
///    (This would translate to "only the minimum and maximum exactly are supported")
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BufferSize {
    pub min: i32,
    pub max: i32,
    pub preferred: i32,
    pub granularity: Option<Granularity>
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Granularity {
    /// Supported buffer sizes increase in linear steps (`range.start` + n * `step`)
    Linear { step: NonZeroI32 },
    /// Supported buffer sizes increase in powers of 2 (min * 2^n)
    Exponential
}

impl From<NonZeroI32> for Granularity {
    fn from(step: NonZeroI32) -> Self {
        if step.get() == -1 {
            Self::Exponential
        } else {
            Self::Linear { step }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelId {
    pub input: bool,
    pub index: sys::ChannelIndex
}