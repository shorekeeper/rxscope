//! Device enumeration and identifier encoding.
//!
//! One list covers both directions. WASAPI exposes input endpoints and output
//! endpoints through the same interface; an output is recorded by opening it
//! with the loopback flag, which is a property of the device, not a separate
//! backend. The legacy interface has inputs only.
//!
//! The identifier written to the configuration carries the direction as a
//! prefix, so a stored selection is unambiguous without consulting the system
//! again at startup:
//!     in:{endpoint}    capture endpoint
//!     out:{endpoint}   render endpoint, opened as loopback
//!     wave:{index}     legacy input
//!     empty            whatever the system considers the default input
//! An identifier without a prefix is read as an endpoint of the direction the
//! backend implies, which is what earlier builds wrote.
//!
//! Endpoints are listed regardless of their state, apart from the ones the
//! system reports as not present. An input with jack detection sits in the
//! unplugged state whenever the socket is empty, and a device switched off in
//! the system panel sits in the disabled state; filtering on the active state
//! alone hides both, which on a typical machine leaves only the line input
//! visible. The state is appended to the label instead.

use std::ffi::c_void;

use crate::config::settings::AudioBackend;
use crate::platform::win32::com::*;
use crate::platform::win32::{mm, string_from_wide_ptr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// Capture endpoint.
    Input,
    /// Render endpoint, recorded through the loopback path.
    Loopback,
    /// Legacy waveIn input.
    Legacy,
    /// Render endpoint used for listening rather than for recording.
    Output,
}

impl DeviceKind {
    fn prefix(self) -> &'static str {
        match self {
            DeviceKind::Input => "in:",
            DeviceKind::Loopback => "out:",
            DeviceKind::Legacy => "wave:",
            // The monitor stores a bare endpoint, so this prefix is never
            // written; it exists so the parser stays total over the enumeration.
            DeviceKind::Output => "",
        }
    }
}

/// Availability of an endpoint, mapped from the system state mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Active,
    Unplugged,
    Disabled,
    Unknown,
}

impl DeviceState {
    fn from_mask(mask: u32) -> DeviceState {
        if mask & DEVICE_STATE_ACTIVE != 0 {
            DeviceState::Active
        } else if mask & DEVICE_STATE_UNPLUGGED != 0 {
            DeviceState::Unplugged
        } else if mask & DEVICE_STATE_DISABLED != 0 {
            DeviceState::Disabled
        } else {
            DeviceState::Unknown
        }
    }

    /// Suffix appended to the label. An active device gets none, so the common
    /// case stays uncluttered.
    fn suffix(self) -> &'static str {
        match self {
            DeviceState::Active => "",
            DeviceState::Unplugged => " (unplugged)",
            DeviceState::Disabled => " (disabled)",
            DeviceState::Unknown => " (unavailable)",
        }
    }

    pub fn usable(self) -> bool {
        self == DeviceState::Active
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Prefixed identifier, stored in the configuration verbatim.
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub state: DeviceState,
    /// True for the synthetic entry that follows the system default.
    pub is_default: bool,
}

/// Decoded identifier.
#[derive(Debug, Clone)]
pub struct DeviceRef {
    pub kind: DeviceKind,
    /// Endpoint string or index text, without the prefix. Empty means default.
    pub raw: String,
}

impl DeviceRef {
    pub fn parse(id: &str, backend: AudioBackend) -> DeviceRef {
        let default_kind = match backend {
            AudioBackend::Wasapi => DeviceKind::Input,
            AudioBackend::WaveIn => DeviceKind::Legacy,
        };
        if id.is_empty() {
            return DeviceRef { kind: default_kind, raw: String::new() };
        }
        for kind in [DeviceKind::Input, DeviceKind::Loopback, DeviceKind::Legacy] {
            if let Some(rest) = id.strip_prefix(kind.prefix()) {
                return DeviceRef { kind, raw: rest.to_string() };
            }
        }
        // No prefix: an identifier written before the encoding existed.
        DeviceRef { kind: default_kind, raw: id.to_string() }
    }

    pub fn is_loopback(&self) -> bool {
        self.kind == DeviceKind::Loopback
    }

    /// Data flow the endpoint belongs to.
    pub fn flow(&self) -> i32 {
        if self.is_loopback() {
            E_RENDER
        } else {
            E_CAPTURE
        }
    }

    pub fn is_default(&self) -> bool {
        self.raw.is_empty()
    }
}

/// Lists everything the backend can open, default entry first.
pub fn enumerate(backend: AudioBackend) -> Vec<DeviceInfo> {
    match backend {
        AudioBackend::Wasapi => wasapi_devices(),
        AudioBackend::WaveIn => legacy_devices(),
    }
}

fn wasapi_devices() -> Vec<DeviceInfo> {
    // Enumeration runs on the caller thread, normally the interface thread,
    // which may already be inside a single threaded apartment.
    let _apartment = Apartment::new(false);

    let mut enumerator: ComPtr<IMMDeviceEnumerator> = ComPtr::null();
    let hr = unsafe {
        CoCreateInstance(
            &CLSID_MMDeviceEnumerator,
            std::ptr::null_mut(),
            CLSCTX_ALL,
            &IID_IMMDeviceEnumerator,
            enumerator.out() as *mut *mut c_void,
        )
    };
    if hr != S_OK || enumerator.is_null() {
        crate::log_warn!("audio", "cannot create the device enumerator, 0x{:08X}", hr as u32);
        return vec![default_entry(DeviceKind::Input, String::new())];
    }

    // The default entry carries the name of whatever it currently resolves to,
    // so the operator can tell which device the empty identifier selects.
    let default_name = unsafe { default_endpoint_name(enumerator.as_raw(), E_CAPTURE) };
    let mut list = vec![default_entry(DeviceKind::Input, default_name)];

    let mut skipped = 0usize;
    // Inputs first: that is what a receiver is normally wired to. Outputs
    // follow, labelled so a loopback selection is obvious in the combo.
    collect(enumerator.as_raw(), E_CAPTURE, DeviceKind::Input, &mut list, &mut skipped);
    collect(enumerator.as_raw(), E_RENDER, DeviceKind::Loopback, &mut list, &mut skipped);

    // Usable devices float to the top of their group; the relative order inside
    // a group is the system one, which is stable across runs.
    list[1..].sort_by_key(|d| (d.kind == DeviceKind::Loopback, !d.state.usable()));

    crate::log_info!(
        "audio",
        "{} endpoints listed, {} skipped as not present",
        list.len() - 1,
        skipped
    );
    list
}

/// Lists the endpoints the monitor can play to, default entry first.
///
/// The identifier is the bare endpoint string rather than a prefixed one. The
/// direction is not in doubt here: this list exists only for the monitor, and
/// the monitor only plays. Keeping the prefix would also make the comparison
/// against a loopback capture identifier, which is how feedback is detected,
/// depend on stripping it back off.
pub fn enumerate_render() -> Vec<DeviceInfo> {
    let _apartment = Apartment::new(false);

    let mut enumerator: ComPtr<IMMDeviceEnumerator> = ComPtr::null();
    let hr = unsafe {
        CoCreateInstance(
            &CLSID_MMDeviceEnumerator,
            std::ptr::null_mut(),
            CLSCTX_ALL,
            &IID_IMMDeviceEnumerator,
            enumerator.out() as *mut *mut c_void,
        )
    };
    if hr != S_OK || enumerator.is_null() {
        crate::log_warn!("audio", "cannot enumerate render endpoints, 0x{:08X}", hr as u32);
        return vec![default_entry(DeviceKind::Output, String::new())];
    }

    let resolved = unsafe { default_endpoint_name(enumerator.as_raw(), E_RENDER) };
    let mut list = vec![default_entry(DeviceKind::Output, resolved)];

    let mut skipped = 0usize;
    collect(enumerator.as_raw(), E_RENDER, DeviceKind::Output, &mut list, &mut skipped);
    list[1..].sort_by_key(|d| !d.state.usable());

    crate::log_info!("audio", "{} render endpoints listed", list.len() - 1);
    list
}

fn collect(
    enumerator: *mut IMMDeviceEnumerator,
    flow: i32,
    kind: DeviceKind,
    list: &mut Vec<DeviceInfo>,
    skipped: &mut usize,
) {
    let mut collection: ComPtr<IMMDeviceCollection> = ComPtr::null();
    let hr = unsafe {
        ((*(*enumerator).vtbl).EnumAudioEndpoints)(
            enumerator,
            flow,
            DEVICE_STATEMASK_ALL,
            collection.out(),
        )
    };
    if hr != S_OK || collection.is_null() {
        crate::log_warn!("audio", "EnumAudioEndpoints failed, 0x{:08X}", hr as u32);
        return;
    }

    let mut count = 0u32;
    unsafe {
        let c = collection.as_raw();
        if ((*(*c).vtbl).GetCount)(c, &mut count) != S_OK {
            return;
        }
    }

    for index in 0..count {
        let mut device: ComPtr<IMMDevice> = ComPtr::null();
        unsafe {
            let c = collection.as_raw();
            if ((*(*c).vtbl).Item)(c, index, device.out()) != S_OK || device.is_null() {
                continue;
            }
        }

        let mask = unsafe { device_state(device.as_raw()) };
        // A device the system reports as not present has no hardware behind it
        // at all; listing it would only offer a guaranteed failure.
        if mask & DEVICE_STATE_NOTPRESENT != 0 {
            *skipped += 1;
            continue;
        }
        let state = DeviceState::from_mask(mask);

        let endpoint = unsafe { device_id(device.as_raw()) };
        if endpoint.is_empty() {
            *skipped += 1;
            continue;
        }
        let friendly = unsafe { device_name(device.as_raw()) };
        let label = if friendly.is_empty() { endpoint.clone() } else { friendly };
        let name = match kind {
            DeviceKind::Loopback => format!("loopback: {}{}", label, state.suffix()),
            _ => format!("{}{}", label, state.suffix()),
        };

        // A render endpoint is stored bare, without the direction prefix the
        // capture kinds carry. The direction is not in doubt for the monitor,
        // which only ever plays, and keeping a prefix would make the comparison
        // that detects feedback depend on stripping it back off first.
        let id = if kind == DeviceKind::Output {
            endpoint.clone()
        } else {
            format!("{}{}", kind.prefix(), endpoint)
        };

        crate::log_debug!("audio", "{:?} {}: {} state {:?}", kind, index, name, state);
        list.push(DeviceInfo { id, name, kind, state, is_default: false });
    }
}

fn default_entry(kind: DeviceKind, resolved: String) -> DeviceInfo {
    let fallback = if kind == DeviceKind::Output {
        "system default output"
    } else {
        "system default input"
    };
    let name = if resolved.is_empty() {
        fallback.to_string()
    } else {
        format!("system default ({})", resolved)
    };
    DeviceInfo { id: String::new(), name, kind, state: DeviceState::Active, is_default: true }
}

/// Friendly name of the endpoint the empty identifier resolves to. A failure
/// only costs the label, so it is not reported as an error.
unsafe fn default_endpoint_name(enumerator: *mut IMMDeviceEnumerator, flow: i32) -> String {
    let mut device: ComPtr<IMMDevice> = ComPtr::null();
    let hr =
        ((*(*enumerator).vtbl).GetDefaultAudioEndpoint)(enumerator, flow, E_CONSOLE, device.out());
    if hr != S_OK || device.is_null() {
        return String::new();
    }
    device_name(device.as_raw())
}

unsafe fn device_state(device: *mut IMMDevice) -> u32 {
    let mut mask = 0u32;
    if ((*(*device).vtbl).GetState)(device, &mut mask) != S_OK {
        return 0;
    }
    mask
}

/// The identifier string is allocated by the callee and must be released with
/// the task allocator.
unsafe fn device_id(device: *mut IMMDevice) -> String {
    let mut raw: *mut u16 = std::ptr::null_mut();
    if ((*(*device).vtbl).GetId)(device, &mut raw) != S_OK || raw.is_null() {
        return String::new();
    }
    let text = string_from_wide_ptr(raw);
    CoTaskMemFree(raw as *mut c_void);
    text
}

/// Reads the friendly name through the property store. A device without one is
/// still usable, so a failure here only costs the label.
unsafe fn device_name(device: *mut IMMDevice) -> String {
    let mut store: ComPtr<IPropertyStore> = ComPtr::null();
    if ((*(*device).vtbl).OpenPropertyStore)(device, STGM_READ, store.out()) != S_OK
        || store.is_null()
    {
        return String::new();
    }

    let mut value = PROPVARIANT::default();
    let s = store.as_raw();
    if ((*(*s).vtbl).GetValue)(s, &PKEY_Device_FriendlyName, &mut value) != S_OK {
        // The variant is cleared even on failure: the callee may have written a
        // partial value before giving up.
        PropVariantClear(&mut value);
        return String::new();
    }

    let text = if value.vt == VT_LPWSTR {
        string_from_wide_ptr(value.as_wide_ptr())
    } else {
        String::new()
    };
    PropVariantClear(&mut value);
    text
}

/// Legacy inputs are identified by index. The mapper entry is offered as the
/// default so a configuration without an explicit device still works.
fn legacy_devices() -> Vec<DeviceInfo> {
    let mut list = vec![default_entry(DeviceKind::Legacy, String::new())];
    let count = unsafe { mm::waveInGetNumDevs() };
    for index in 0..count {
        let name = mm::device_name(index).unwrap_or_else(|| format!("input {}", index));
        crate::log_debug!("audio", "waveIn {}: {}", index, name);
        list.push(DeviceInfo {
            id: format!("{}{}", DeviceKind::Legacy.prefix(), index),
            name,
            kind: DeviceKind::Legacy,
            // The legacy interface exposes no state; anything it enumerates is
            // openable in principle.
            state: DeviceState::Active,
            is_default: false,
        });
    }
    list
}