use std::ffi::{CString, c_long, c_void};
use std::mem::MaybeUninit;
use std::num::NonZeroI32;
use std::{mem, ptr};
use crate::{WinResult, dto, sys};
use crate::dto::Granularity;
use crate::future::AsioFuture;
use crate::utils::{COMInit, ResultCodeExt, cstring_from_bytes_until_nul};
use crate::win::{CLSCTX_SERVER, E_POINTER, HWND};
use sys::IIASIORedecl;
use tap::Pipe;
use windows_core::{GUID, HSTRING};

#[cfg(feature = "host")]
pub use crate::host::Proxy;

/// The spec's size of a driver, channel or clock source name buffer, NUL included.
const NAME_LEN: usize = 32;

/// Room for clock sources in one [`Driver::clock_sources`] call.
pub const MAX_CLOCK_SOURCES: usize = 64;

pub trait Driver {
	/// The spec unfortunately does not elaborate on the purpose of the parameter.
	#[must_use]
	fn init(&self, window_handle: Option<HWND>) -> bool;

	/// Usually (but not necessarily) the same as [`DriverMetadata::description`].
	#[must_use]
	fn name(&self) -> CString;
	
	/// Intended to be the major ASIO version (`2` since the release of ASIO 2.0 in 1999),
	/// but technically allowed to be higher by spec, and many report their own (independent) version this way.
	#[must_use]
	fn version(&self) -> sys::DriverVersion;
	
	/// Retrieves a message associated with the recentmost error.
	#[must_use]
	fn last_error(&self) -> CString;
	
	/// Drivers typically invoke the [`buffer_switch`](sys::Callbacks::buffer_switch) / [`buffer_switch_time_info`](sys::Callbacks::buffer_switch_time_info)
	/// callback 1+ times during (or immediately after) this function call to prime the output buffer(s).
	fn start(&self) -> crate::Result<()>;
	
	/// Halts the streaming.<br>
	/// The driver remains ready to resume via [`.start()`](Self::start).
	fn stop(&self) -> crate::Result<()>;
	
	/// Returns the number of channels in each direction.
	fn channel_counts(&self) -> crate::Result<dto::ChannelCounts>;
	
	/// Accounts for buffer size, assuming [`BufferSize::preferred`](dto::BufferSize::preferred)
	/// when called before [`.create_buffers()`](Self::create_buffers).
	fn latencies(&self) -> crate::Result<dto::Latencies>;
	
	/// Retrieves buffer size(s) supported by the driver.<br>
	/// These can depend on the current sample rate.
	fn buffer_size(&self) -> crate::Result<dto::BufferSize>;
	
	/// Checks whether the specified `sample_rate` is supported.
	fn can_sample_rate(&self, sample_rate: sys::SampleRate) -> crate::Result<()>;
	
	/// Returns the current sample rate.
	fn get_sample_rate(&self) -> crate::Result<sys::SampleRate>;
		
	/// 0 = external sync
	fn set_sample_rate(&self, sample_rate: sys::SampleRate) -> crate::Result<()>;
	
	/// Retrieves a list of all clock sources available to this driver.
	fn clock_sources(&self) -> crate::Result<Vec<dto::ClockSourceResponse>>;
	
	/// Selects a [`ClockSource`](sys::ClockSource), as enumerated via [`.clock_sources()`](Self::clock_sources)
	fn set_clock_source(&self, clock_source: sys::ClockSourceIndex) -> crate::Result<()>;
	
	/// Tells the driver to open its GUI
	fn sample_position(&self) -> crate::Result<dto::SamplePosition>;

	fn channel_info(&self, channel_id: dto::ChannelId) -> crate::Result<dto::ChannelInfoResponse>;

	fn dispose_buffers(&self) -> crate::Result<()>;
	
	/// Tells the driver to open its GUI
	fn open_control_panel(&self) -> crate::Result<()>;
	
	/// Tells the driver that the host is done processing output buffers.
	/// 
	/// This is *not* implicitly inferred from the return of [`Callbacks::buffer_switch`] / [`Callbacks::buffer_switch_time_info`],
	/// because it might have been called by a thread that doesn't allow processing within the callback.
	/// 
	/// # Caveats
	/// Devices without hardware DSP and no further internal buffering
	/// have no use for this signal, so their drivers might not support it,
	/// and instead return [`ResultCode::NOT_PRESENT`].
	/// This is not fatal, it just means that calls to this function can (and should) be skipped.
	/// Take care not to "error out" unnecessarily in this case.
	fn output_ready(&self) -> crate::Result<()>;
	
	/// # Safety
	/// * `callbacks` must outlive the created buffers.
	/// * Derefs of the returned buffer pointers must not.
	/// # Remarks
	/// This function is kept C-style because providing safe abstractions for it
	/// is very difficult to do without getting highly opinionated. (Help wanted!)
	unsafe fn create_buffers(
		&self,
		channels: impl IntoIterator<Item=dto::ChannelId>,
		buffer_size: c_long,
		callbacks: *const sys::Callbacks
	) -> crate::Result<impl Iterator<Item=[*mut c_void; 2]>>;
	
	/// A very unfortunate name. 
	/// This function actually has nothing to do with async code,
	/// it merely provides a mechanism for extending ASIO in the future.
	fn future<T: AsioFuture>(&self, param: &mut T::Param) -> crate::Result<()>;
}

/// Metadata of an ASIO driver, retrieved from the system registry via [`get_drivers`]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Metadata {
	pub clsid: GUID,
	pub description: HSTRING,
}

impl Metadata {
	/// Gathers the metadata of all ASIO drivers currently registered in the system.
	/// Malformed keys are skipped.
	/// 
	/// This is the "starting point" of this library.
	pub fn enumerate() -> WinResult<Vec<Self>> {
		let software_key = windows_registry::LOCAL_MACHINE.open("SOFTWARE\\ASIO")?;
			
		let drivers =
			software_key
			.keys()?
			.filter_map(|driver_key_name| {
				let driver_key = software_key.open(&driver_key_name).ok()?;
				Self::from_registry(&driver_key).ok()
			})
			.collect();
		
		Ok(drivers)
	}
	
	fn from_registry(key: &windows_registry::Key) -> WinResult<Self> {
		let clsid =
			key
			.get_string("clsid")?
			.trim_matches(['{', '}'])
			.try_into()?;
		
		let description =
			key
			.get_hstring("description")?;
		
		Ok(Self { clsid, description })
	}
}

/// A safe, [`Clone`]able handle to a driver instance.
/// This type is ! [`Send`] because the driver instance lives in a single-threaded COM apartment.
/// [`crate::Host`] provides the necessary machinery to get around this limitation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeHandle(UnsafeHandle, COMInit); // significant drop-order!

impl SafeHandle {
	pub fn new(clsid: &GUID) -> WinResult<Self> {
		let com = COMInit::new()?;
		let inner = unsafe { UnsafeHandle::new(clsid) }?;
		Ok(Self(inner, com))
	}
	
	/// # Safety
	/// The caller must ensure that the returned handle and derivatives do not outlive the driver instance.
	#[must_use]
	pub const unsafe fn as_unsafe(&self) -> &UnsafeHandle {
		&self.0
	}
}

// can't use Deref as that would provide access to its `Clone` implementation without ever having to commit to the API contract
impl Driver for SafeHandle {
	fn init              (&self, window_handle: Option<HWND>        ) -> bool                                    { self.0.init              (window_handle) }
	fn name              (&self                                     ) -> CString                                 { self.0.name              (             ) }
	fn version           (&self                                     ) -> sys::DriverVersion                      { self.0.version           (             ) }
	fn last_error        (&self                                     ) -> CString                                 { self.0.last_error        (             ) }
	fn start             (&self                                     ) -> crate::Result<()>                       { self.0.start             (             ) }
	fn stop              (&self                                     ) -> crate::Result<()>                       { self.0.stop              (             ) }
	fn channel_counts    (&self                                     ) -> crate::Result<dto::ChannelCounts>       { self.0.channel_counts    (             ) }
	fn latencies         (&self                                     ) -> crate::Result<dto::Latencies>           { self.0.latencies         (             ) }
	fn buffer_size       (&self                                     ) -> crate::Result<dto::BufferSize>          { self.0.buffer_size       (             ) }
	fn can_sample_rate   (&self, sample_rate: sys::SampleRate       ) -> crate::Result<()>                       { self.0.can_sample_rate   (sample_rate  ) }
	fn get_sample_rate   (&self                                     ) -> crate::Result<sys::SampleRate>          { self.0.get_sample_rate   (             ) }
	fn set_sample_rate   (&self, sample_rate: sys::SampleRate       ) -> crate::Result<()>                       { self.0.set_sample_rate   (sample_rate  ) }
	fn clock_sources     (&self                                     ) -> crate::Result<Vec<dto::ClockSourceResponse>> { self.0.clock_sources     (             ) }
	fn set_clock_source  (&self, clock_source: sys::ClockSourceIndex) -> crate::Result<()>                       { self.0.set_clock_source  (clock_source ) }
	fn sample_position   (&self                                     ) -> crate::Result<dto::SamplePosition>      { self.0.sample_position   (             ) }
	fn channel_info      (&self, channel_id: dto::ChannelId         ) -> crate::Result<dto::ChannelInfoResponse> { self.0.channel_info      (channel_id   ) }
	fn dispose_buffers   (&self                                     ) -> crate::Result<()>                       { self.0.dispose_buffers   (             ) }
	fn open_control_panel(&self                                     ) -> crate::Result<()>                       { self.0.open_control_panel(             ) }
	fn output_ready      (&self                                     ) -> crate::Result<()>                       { self.0.output_ready      (             ) }
	
	unsafe fn create_buffers(
		&self,
		channels   : impl IntoIterator<Item=dto::ChannelId>,
		buffer_size: c_long,
		callbacks  : *const azo_sys::Callbacks
	) -> crate::Result<impl Iterator<Item=[*mut c_void; 2]>> {
		unsafe { self.0.create_buffers(channels, buffer_size, callbacks) }
	}
	
	fn future<T: AsioFuture>(&self, param: &mut T::Param) -> crate::Result<()> {
		self.0.future::<T>(param)
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsafeHandle(pub IIASIORedecl);

impl UnsafeHandle {
	/// # Safety
	/// The caller must ensure that COM
	/// * is initialized on this thread
	/// * stays that way until this handle and all its clones got dropped
	pub unsafe fn new(guid: &GUID) -> WinResult<Self> {
		// The CLSID doubles as the IID: every ASIO driver answers `QueryInterface` for its own
		// CLSID (the SDK's host code creates drivers this way), and asking for it directly
		// hands over the single reference `CoCreateInstance` made. Creating an `IUnknown` and
		// querying it instead releases a reference before `init`, and some drivers (wineasio
		// 1.3.0) read state in `Release` that `init` has not set yet.
		// windows-rs binds `CoCreateInstance` with the IID taken from the target type, which
		// `IIASIORedecl` cannot provide (see its doc comment), hence the raw import.
		windows_core::link!("ole32.dll" "system" fn CoCreateInstance(rclsid: *const GUID, punkouter: *mut c_void, dwclscontext: u32, riid: *const GUID, ppv: *mut *mut c_void) -> windows_core::HRESULT);
		let mut raw = ptr::null_mut();
		unsafe { CoCreateInstance(guid, ptr::null_mut(), CLSCTX_SERVER as _, guid, &raw mut raw) }.ok()?;
		if raw.is_null() {
			return Err(E_POINTER.into());
		}
		// SAFETY: `raw` is a non-null interface pointer of the requested IID, owning one reference.
		Ok(Self(unsafe { <IIASIORedecl as windows_core::Interface>::from_raw(raw) }))
	}

	#[must_use]
	pub const fn from_raw(raw: IIASIORedecl) -> Self {
		Self(raw)
	}
}

impl Driver for UnsafeHandle {    
	fn init(&self, main_window_handle: Option<HWND>) -> bool {
		let sys_ref = main_window_handle.unwrap_or_default(); 

		unsafe { self.0.init(sys_ref.0) }
		.try_into()
		.unwrap_or(false)
	}

	fn name(&self) -> CString {
		// The spec allows 32 bytes, NUL included, but drivers that write a 32-character name
		// put its NUL one past the end; the room after the first 32 bytes takes that overrun.
		let mut buf = [0_u8; 2 * NAME_LEN];
		unsafe { self.0.get_driver_name(buf.as_mut_ptr()); }
		cstring_from_bytes_until_nul(&buf[..NAME_LEN])
	}

	fn version(&self) -> sys::DriverVersion {
		unsafe { self.0.get_driver_version() }
	}

	fn last_error(&self) -> CString {
		let mut buf = [0_u8; 124];
		unsafe { self.0.get_error_message(buf.as_mut_ptr()); }
		cstring_from_bytes_until_nul(&buf)
	}
	
	fn start(&self) -> crate::Result<()> {
		unsafe { self.0.start() }
		.to_result()
	}
	fn stop(&self) -> crate::Result<()> {
		unsafe { self.0.stop() }
		.to_result()
	}

	fn channel_counts(&self) -> crate::Result<dto::ChannelCounts> {
		let mut in_ = MaybeUninit::uninit();
		let mut out = MaybeUninit::uninit();
		
		unsafe { self.0.get_channels(in_.as_mut_ptr(), out.as_mut_ptr()) }
    	.to_result_with(|| dto::ChannelCounts {
			in_: unsafe { in_.assume_init() },
			out: unsafe { out.assume_init() }
		})
	}

	fn latencies(&self) -> crate::Result<dto::Latencies> {
		let mut in_ = MaybeUninit::uninit();
		let mut out = MaybeUninit::uninit();
		
		unsafe { self.0.get_latencies(in_.as_mut_ptr(), out.as_mut_ptr()) }
    	.to_result_with(|| dto::Latencies {
			in_: unsafe { in_.assume_init() },
			out: unsafe { out.assume_init() }
		})
	}

	fn buffer_size(&self) -> crate::Result<dto::BufferSize> {
		let mut min         = MaybeUninit::uninit();
		let mut max         = MaybeUninit::uninit();
		let mut preferred   = MaybeUninit::uninit();
		let mut granularity = MaybeUninit::uninit();
		
		unsafe {
			self.0.get_buffer_size(
				min.as_mut_ptr(),
				max.as_mut_ptr(),
				preferred.as_mut_ptr(),
				granularity.as_mut_ptr()
			)
		}
    	.to_result()
    	.map(|()| dto::BufferSize {
			min        : unsafe { min        .assume_init() },
			max        : unsafe { max        .assume_init() },
			preferred  : unsafe { preferred  .assume_init() },
			granularity: unsafe { granularity.assume_init() }
				.pipe(NonZeroI32::new)
				.map(Granularity::from)
		})
	}

	fn can_sample_rate(&self, sample_rate: sys::SampleRate) -> crate::Result<()> {
		unsafe { self.0.can_sample_rate(sample_rate) }
    	.to_result()
	}
	
	fn get_sample_rate(&self) -> crate::Result<sys::SampleRate> {
		let mut sample_rate = MaybeUninit::uninit();

		unsafe { self.0.get_sample_rate(sample_rate.as_mut_ptr()) }
    	.to_result_with(|| unsafe { sample_rate.assume_init() })
	}

	fn set_sample_rate(&self, sample_rate: sys::SampleRate) -> crate::Result<()> {
		unsafe { self.0.set_sample_rate(sample_rate) }
		.to_result()
	}

	/// Asks once, with room for [`MAX_CLOCK_SOURCES`] and a spare entry after them: drivers
	/// that ignore the requested count and write every source they have would overrun a
	/// smaller array, and a 32-character name's NUL lands past the last entry's name field.
	fn clock_sources(&self) -> crate::Result<Vec<dto::ClockSourceResponse>> {
		let mut all = Vec::<sys::ClockSource>::with_capacity(MAX_CLOCK_SOURCES + 1);
		let mut count = MAX_CLOCK_SOURCES as c_long; // on input this is the # of requested items
		
		unsafe { self.0.get_clock_sources(all.spare_capacity_mut().as_mut_ptr().cast(), &raw mut count) }
		.to_result()?;
		
		let count = usize::try_from(count).unwrap_or(0).min(MAX_CLOCK_SOURCES);
		unsafe { all.set_len(count); }
		Ok(all.into_iter().map(Into::into).collect())
	}

	/// Selects a [`ClockSource`](sys::ClockSource), as enumerated via [`.clock_sources()`](Self::clock_sources)
	fn set_clock_source(&self, clock_source: sys::ClockSourceIndex) -> crate::Result<()> {
		unsafe { self.0.set_clock_source(clock_source) }
    	.to_result()
	}

	fn sample_position(&self) -> crate::Result<dto::SamplePosition> {
		let mut position   = MaybeUninit::uninit();
		let mut time_stamp = MaybeUninit::uninit();
		
		unsafe { self.0.get_sample_position(position.as_mut_ptr(), time_stamp.as_mut_ptr()) }
		.to_result_with(|| dto::SamplePosition {
			position  : unsafe { position  .assume_init() }.into(),
			time_stamp: unsafe { time_stamp.assume_init() }.into()
		})
	}

	fn channel_info(&self, channel_id: dto::ChannelId) -> crate::Result<dto::ChannelInfoResponse> {
		// A spare entry after the one asked about takes a driver that writes its name's NUL
		// past the 32-byte field, which is the struct's last.
		let mut infos = [
			sys::ChannelInfo {
				channel: channel_id.index,
				is_input: channel_id.input.into(),
				..unsafe { mem::zeroed() }
			},
			unsafe { mem::zeroed() }
		];
		unsafe { self.0.get_channel_info(infos.as_mut_ptr()) }
    	.to_result_with(|| {
			let [info, _spare] = infos;
			info.into()
		})
	}

	unsafe fn create_buffers(
		&self,
		channels: impl IntoIterator<Item=dto::ChannelId>,
		buffer_size: c_long,
		callbacks: *const sys::Callbacks
	) -> crate::Result<impl Iterator<Item=[*mut c_void; 2]>> {
		let mut infos =
			channels
			.into_iter()
			.map(|dto::ChannelId { input, index }|
				sys::BufferInfo {
				    is_input: input.into(),
				    channel_num: index,
				    buffers: [ptr::null_mut(); 2]
				}
			)
			.collect::<Vec<_>>();
		
		unsafe { self.0.create_buffers(infos.as_mut_ptr(), infos.len() as _, buffer_size, callbacks.cast_mut()) }
    	.to_result_with(||
			infos
			.into_iter()
			.map(|info| info.buffers)
		)
	}

	fn dispose_buffers(&self) -> crate::Result<()> {
		unsafe { self.0.dispose_buffers() }
    	.to_result()
	}

	fn open_control_panel(&self) -> crate::Result<()> {
		unsafe { self.0.control_panel() }
		.to_result()
	}

	fn future<T: AsioFuture>(&self, param: &mut T::Param) -> crate::Result<()> {
		let selector = T::SELECTOR;
		let opt = ptr::from_mut(param).cast();
		
		unsafe { self.0.future(selector, opt) }
		.to_result()
	}
	
	fn output_ready(&self) -> crate::Result<()> {
		unsafe { self.0.output_ready() }
		.to_result()
	}
}