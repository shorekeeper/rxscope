//! COM plumbing and the WASAPI interfaces.
//!
//! The interfaces are declared as explicit virtual tables. A COM object is a
//! pointer to a pointer to a table of function pointers, and the tables of
//! derived interfaces start with the table of the base interface, so the
//! layout below mirrors the headers exactly. Method order matters and must
//! not be changed.
//!
//! Only the methods the capture path calls are declared with real signatures;
//! the rest are typed as opaque pointers to keep the table the right size,
//! because calling method n means indexing slot n.

#![allow(non_snake_case, non_camel_case_types, dead_code)]

use std::ffi::c_void;

pub type HRESULT = i32;
/// Hundred nanosecond units, the WASAPI time base.
pub type REFERENCE_TIME = i64;

pub const S_OK: HRESULT = 0;
pub const S_FALSE: HRESULT = 1;
pub const RPC_E_CHANGED_MODE: HRESULT = 0x8001_0106_u32 as i32;

pub const COINIT_APARTMENTTHREADED: u32 = 0x2;
pub const COINIT_MULTITHREADED: u32 = 0x0;
pub const CLSCTX_ALL: u32 = 0x17;
pub const STGM_READ: u32 = 0;

/// Success code returned by GetBuffer when no data is pending.
pub const AUDCLNT_S_BUFFER_EMPTY: HRESULT = 0x0889_0001;
pub const AUDCLNT_E_UNSUPPORTED_FORMAT: HRESULT = 0x8889_0008_u32 as i32;
pub const AUDCLNT_E_DEVICE_INVALIDATED: HRESULT = 0x8889_0004_u32 as i32;
pub const AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED: HRESULT = 0x8889_0019_u32 as i32;
pub const AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED: HRESULT = 0x8889_000A_u32 as i32;

pub const AUDCLNT_SHAREMODE_SHARED: i32 = 0;
pub const AUDCLNT_SHAREMODE_EXCLUSIVE: i32 = 1;

pub const AUDCLNT_STREAMFLAGS_LOOPBACK: u32 = 0x0002_0000;
pub const AUDCLNT_STREAMFLAGS_EVENTCALLBACK: u32 = 0x0004_0000;

pub const AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY: u32 = 0x1;
pub const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 0x2;
pub const AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR: u32 = 0x4;

/// Data flow direction of an endpoint.
pub const E_RENDER: i32 = 0;
pub const E_CAPTURE: i32 = 1;

/// Endpoint role, used only for the default device query.
pub const E_CONSOLE: i32 = 0;

/// Endpoint states. An input without jack detection is always active; one with
/// detection reports unplugged while the socket is empty, and a device switched
/// off in the system panel reports disabled. Both can still be listed, they
/// simply cannot be opened until the operator fixes the cause.
pub const DEVICE_STATE_ACTIVE: u32 = 0x1;
pub const DEVICE_STATE_DISABLED: u32 = 0x2;
pub const DEVICE_STATE_NOTPRESENT: u32 = 0x4;
pub const DEVICE_STATE_UNPLUGGED: u32 = 0x8;
pub const DEVICE_STATEMASK_ALL: u32 = 0xF;

pub const WAVE_FORMAT_PCM: u16 = 1;
pub const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
pub const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// VT_LPWSTR, the only property variant type the enumeration reads.
pub const VT_LPWSTR: u16 = 31;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GUID {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

pub const CLSID_MMDeviceEnumerator: GUID = GUID {
    data1: 0xBCDE_0395,
    data2: 0xE52F,
    data3: 0x467C,
    data4: [0x8E, 0x3D, 0xC4, 0x57, 0x92, 0x91, 0x69, 0x2E],
};

pub const IID_IMMDeviceEnumerator: GUID = GUID {
    data1: 0xA956_64D2,
    data2: 0x9614,
    data3: 0x4F35,
    data4: [0xA7, 0x46, 0xDE, 0x8D, 0xB6, 0x36, 0x17, 0xE6],
};

pub const IID_IAudioClient: GUID = GUID {
    data1: 0x1CB9_AD4C,
    data2: 0xDBFA,
    data3: 0x4C32,
    data4: [0xB1, 0x78, 0xC2, 0xF5, 0x68, 0xA7, 0x03, 0xB2],
};

pub const IID_IAudioCaptureClient: GUID = GUID {
    data1: 0xC8AD_BD64,
    data2: 0xE71E,
    data3: 0x48A0,
    data4: [0xA4, 0xDE, 0x18, 0x5C, 0x39, 0x5C, 0xD3, 0x17],
};

pub const IID_IAudioRenderClient: GUID = GUID {
    data1: 0xF294_ACFC,
    data2: 0x3146,
    data3: 0x4483,
    data4: [0xA7, 0xBF, 0xAD, 0xDC, 0xA7, 0xC2, 0x60, 0xE2],
};

pub const KSDATAFORMAT_SUBTYPE_PCM: GUID = GUID {
    data1: 0x0000_0001,
    data2: 0x0000,
    data3: 0x0010,
    data4: [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
};

pub const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID = GUID {
    data1: 0x0000_0003,
    data2: 0x0000,
    data3: 0x0010,
    data4: [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PROPERTYKEY {
    pub fmtid: GUID,
    pub pid: u32,
}

/// Friendly device name, the string shown in the device combo.
pub const PKEY_Device_FriendlyName: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID {
        data1: 0xA45C_254E,
        data2: 0xDF1C,
        data3: 0x4EFD,
        data4: [0x80, 0x20, 0x67, 0xD1, 0x46, 0xA8, 0x50, 0xE0],
    },
    pid: 14,
};

/// Property variant. The real union is larger than anything read here, so the
/// payload is declared as two words: that covers the pointer case and keeps
/// the structure at least as large as the system expects.
#[repr(C)]
pub struct PROPVARIANT {
    pub vt: u16,
    pub pad1: u16,
    pub pad2: u16,
    pub pad3: u16,
    pub data: [u64; 2],
}

impl Default for PROPVARIANT {
    fn default() -> PROPVARIANT {
        PROPVARIANT { vt: 0, pad1: 0, pad2: 0, pad3: 0, data: [0, 0] }
    }
}

impl PROPVARIANT {
    /// Reads the inline pointer as a wide string pointer. Valid only when vt
    /// says the payload is a string.
    pub unsafe fn as_wide_ptr(&self) -> *const u16 {
        *(self.data.as_ptr() as *const *const u16)
    }
}

/// Format descriptor.
///
/// The platform headers wrap the wave format structures in byte packing, which
/// is why this one is eighteen bytes and not twenty. Without the packing
/// attribute the compiler appends two bytes of tail padding to satisfy the four
/// byte alignment of the rate fields, every field of the extension below shifts
/// by four, and the subformat identifier is read from the wrong offset.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct WAVEFORMATEX {
    pub wFormatTag: u16,
    pub nChannels: u16,
    pub nSamplesPerSec: u32,
    pub nAvgBytesPerSec: u32,
    pub nBlockAlign: u16,
    pub wBitsPerSample: u16,
    /// Size of the extension that follows, zero for a plain WAVEFORMATEX.
    pub cbSize: u16,
}

/// Extension used by every modern shared mode mix format. The union in the
/// headers is declared as a single word; only the valid bit count is ever read
/// from it, so the alternative names are omitted.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct WAVEFORMATEXTENSIBLE {
    pub Format: WAVEFORMATEX,
    /// Valid bits per sample, container size is in Format.wBitsPerSample.
    pub wValidBitsPerSample: u16,
    pub dwChannelMask: u32,
    pub SubFormat: GUID,
}

// Layout guards. A mismatch would misparse every device format, and the symptom
// would be a rejected subformat rather than a compile failure, so the sizes are
// checked here instead.
const _: () = assert!(std::mem::size_of::<WAVEFORMATEX>() == 18);
const _: () = assert!(std::mem::size_of::<WAVEFORMATEXTENSIBLE>() == 40);

#[repr(C)]
pub struct IUnknownV {
    pub QueryInterface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    pub AddRef: unsafe extern "system" fn(*mut c_void) -> u32,
    pub Release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
pub struct IMMDeviceEnumerator {
    pub vtbl: *const IMMDeviceEnumeratorV,
}

#[repr(C)]
pub struct IMMDeviceEnumeratorV {
    pub base: IUnknownV,
    pub EnumAudioEndpoints: unsafe extern "system" fn(
        *mut IMMDeviceEnumerator,
        i32,
        u32,
        *mut *mut IMMDeviceCollection,
    ) -> HRESULT,
    pub GetDefaultAudioEndpoint:
        unsafe extern "system" fn(*mut IMMDeviceEnumerator, i32, i32, *mut *mut IMMDevice) -> HRESULT,
    pub GetDevice:
        unsafe extern "system" fn(*mut IMMDeviceEnumerator, *const u16, *mut *mut IMMDevice) -> HRESULT,
    pub RegisterEndpointNotificationCallback:
        unsafe extern "system" fn(*mut IMMDeviceEnumerator, *mut c_void) -> HRESULT,
    pub UnregisterEndpointNotificationCallback:
        unsafe extern "system" fn(*mut IMMDeviceEnumerator, *mut c_void) -> HRESULT,
}

#[repr(C)]
pub struct IMMDeviceCollection {
    pub vtbl: *const IMMDeviceCollectionV,
}

#[repr(C)]
pub struct IMMDeviceCollectionV {
    pub base: IUnknownV,
    pub GetCount: unsafe extern "system" fn(*mut IMMDeviceCollection, *mut u32) -> HRESULT,
    pub Item: unsafe extern "system" fn(*mut IMMDeviceCollection, u32, *mut *mut IMMDevice) -> HRESULT,
}

#[repr(C)]
pub struct IMMDevice {
    pub vtbl: *const IMMDeviceV,
}

#[repr(C)]
pub struct IMMDeviceV {
    pub base: IUnknownV,
    pub Activate: unsafe extern "system" fn(
        *mut IMMDevice,
        *const GUID,
        u32,
        *mut PROPVARIANT,
        *mut *mut c_void,
    ) -> HRESULT,
    pub OpenPropertyStore:
        unsafe extern "system" fn(*mut IMMDevice, u32, *mut *mut IPropertyStore) -> HRESULT,
    pub GetId: unsafe extern "system" fn(*mut IMMDevice, *mut *mut u16) -> HRESULT,
    pub GetState: unsafe extern "system" fn(*mut IMMDevice, *mut u32) -> HRESULT,
}

#[repr(C)]
pub struct IPropertyStore {
    pub vtbl: *const IPropertyStoreV,
}

#[repr(C)]
pub struct IPropertyStoreV {
    pub base: IUnknownV,
    pub GetCount: unsafe extern "system" fn(*mut IPropertyStore, *mut u32) -> HRESULT,
    pub GetAt: unsafe extern "system" fn(*mut IPropertyStore, u32, *mut PROPERTYKEY) -> HRESULT,
    pub GetValue:
        unsafe extern "system" fn(*mut IPropertyStore, *const PROPERTYKEY, *mut PROPVARIANT) -> HRESULT,
    pub SetValue:
        unsafe extern "system" fn(*mut IPropertyStore, *const PROPERTYKEY, *const PROPVARIANT) -> HRESULT,
    pub Commit: unsafe extern "system" fn(*mut IPropertyStore) -> HRESULT,
}

#[repr(C)]
pub struct IAudioClient {
    pub vtbl: *const IAudioClientV,
}

#[repr(C)]
pub struct IAudioClientV {
    pub base: IUnknownV,
    pub Initialize: unsafe extern "system" fn(
        *mut IAudioClient,
        i32,
        u32,
        REFERENCE_TIME,
        REFERENCE_TIME,
        *const WAVEFORMATEX,
        *const GUID,
    ) -> HRESULT,
    pub GetBufferSize: unsafe extern "system" fn(*mut IAudioClient, *mut u32) -> HRESULT,
    pub GetStreamLatency: unsafe extern "system" fn(*mut IAudioClient, *mut REFERENCE_TIME) -> HRESULT,
    pub GetCurrentPadding: unsafe extern "system" fn(*mut IAudioClient, *mut u32) -> HRESULT,
    pub IsFormatSupported: unsafe extern "system" fn(
        *mut IAudioClient,
        i32,
        *const WAVEFORMATEX,
        *mut *mut WAVEFORMATEX,
    ) -> HRESULT,
    pub GetMixFormat: unsafe extern "system" fn(*mut IAudioClient, *mut *mut WAVEFORMATEX) -> HRESULT,
    pub GetDevicePeriod: unsafe extern "system" fn(
        *mut IAudioClient,
        *mut REFERENCE_TIME,
        *mut REFERENCE_TIME,
    ) -> HRESULT,
    pub Start: unsafe extern "system" fn(*mut IAudioClient) -> HRESULT,
    pub Stop: unsafe extern "system" fn(*mut IAudioClient) -> HRESULT,
    pub Reset: unsafe extern "system" fn(*mut IAudioClient) -> HRESULT,
    pub SetEventHandle: unsafe extern "system" fn(*mut IAudioClient, *mut c_void) -> HRESULT,
    pub GetService:
        unsafe extern "system" fn(*mut IAudioClient, *const GUID, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
pub struct IAudioCaptureClient {
    pub vtbl: *const IAudioCaptureClientV,
}

#[repr(C)]
pub struct IAudioCaptureClientV {
    pub base: IUnknownV,
    pub GetBuffer: unsafe extern "system" fn(
        *mut IAudioCaptureClient,
        *mut *mut u8,
        *mut u32,
        *mut u32,
        *mut u64,
        *mut u64,
    ) -> HRESULT,
    pub ReleaseBuffer: unsafe extern "system" fn(*mut IAudioCaptureClient, u32) -> HRESULT,
    pub GetNextPacketSize: unsafe extern "system" fn(*mut IAudioCaptureClient, *mut u32) -> HRESULT,
}


#[repr(C)]
pub struct IAudioRenderClient {
    pub vtbl: *const IAudioRenderClientV,
}

#[repr(C)]
pub struct IAudioRenderClientV {
    pub base: IUnknownV,
    pub GetBuffer: unsafe extern "system" fn(*mut IAudioRenderClient, u32, *mut *mut u8) -> HRESULT,
    pub ReleaseBuffer: unsafe extern "system" fn(*mut IAudioRenderClient, u32, u32) -> HRESULT,
}

#[link(name = "ole32")]
extern "system" {
    pub fn CoInitializeEx(pvReserved: *mut c_void, dwCoInit: u32) -> HRESULT;
    pub fn CoUninitialize();
    pub fn CoCreateInstance(
        rclsid: *const GUID,
        pUnkOuter: *mut c_void,
        dwClsContext: u32,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT;
    pub fn CoTaskMemFree(pv: *mut c_void);
    pub fn PropVariantClear(pvar: *mut PROPVARIANT) -> HRESULT;
}

/// Owning interface pointer. Release is reached through the base table, which
/// is the first member of every derived table, so the cast is layout safe.
pub struct ComPtr<T>(*mut T);

impl<T> ComPtr<T> {
    pub fn null() -> ComPtr<T> {
        ComPtr(std::ptr::null_mut())
    }

    /// Takes ownership of a pointer the callee already added a reference to.
    pub unsafe fn from_raw(p: *mut T) -> ComPtr<T> {
        ComPtr(p)
    }

    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }

    pub fn as_raw(&self) -> *mut T {
        self.0
    }

    /// Address to hand to a method that returns an interface.
    pub fn out(&mut self) -> *mut *mut T {
        &mut self.0
    }
}

impl<T> Drop for ComPtr<T> {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let table = self.0 as *mut *const IUnknownV;
                ((**table).Release)(self.0 as *mut c_void);
            }
            self.0 = std::ptr::null_mut();
        }
    }
}

/// Scoped COM apartment.
///
/// CoUninitialize is only called when this object performed the matching
/// initialization. A thread that was already inside an apartment of the other
/// kind reports RPC_E_CHANGED_MODE; the existing apartment stays usable for
/// the device enumerator, so that case is not an error.
pub struct Apartment {
    owned: bool,
}

impl Apartment {
    pub fn new(multithreaded: bool) -> Apartment {
        let mode = if multithreaded { COINIT_MULTITHREADED } else { COINIT_APARTMENTTHREADED };
        let hr = unsafe { CoInitializeEx(std::ptr::null_mut(), mode) };
        let owned = hr == S_OK || hr == S_FALSE;
        if !owned && hr != RPC_E_CHANGED_MODE {
            crate::log_warn!("audio", "CoInitializeEx returned 0x{:08X}", hr as u32);
        }
        Apartment { owned }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.owned {
            unsafe { CoUninitialize() };
        }
    }
}

/// Frees a format block allocated by GetMixFormat.
pub struct MixFormat(pub *mut WAVEFORMATEX);

impl Drop for MixFormat {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CoTaskMemFree(self.0 as *mut c_void) };
            self.0 = std::ptr::null_mut();
        }
    }
}