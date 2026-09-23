//! Sound output.
//!
//! One render endpoint, one thread, one generator. There is no capture path and
//! no monitor: a trainer produces the signal, so the whole of the audio layer is
//! one direction.

pub mod output;

pub use output::{OutputConfig, OutputStatus, OutputStream};

use crate::platform::win32::com::*;
use crate::platform::win32::{string_from_wide_ptr, wide};

/// One render endpoint as the interface lists it.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Identifier the endpoint is reopened by. Empty for the system default.
    ///
    /// The identifier rather than the name, because a name is not unique: two
    /// identical interfaces report the same friendly name and an operator who
    /// chose one would get whichever the enumeration happened to list first.
    pub id: String,
    /// What the panel shows. A localization key for the default entry, so it
    /// follows the language.
    pub name: String,
}

/// Lists the endpoints, the system default first.
///
/// The default is listed rather than merely implied by an empty identifier: an
/// operator who has not chosen anything has to be able to see what they will
/// get, and an endpoint that is currently the default may not be tomorrow.
///
/// Only active endpoints appear. A disabled or unplugged one is listed by the
/// system and cannot be opened, so offering it would be offering a choice that
/// fails.
pub fn enumerate() -> Vec<DeviceInfo> {
    let mut out = vec![DeviceInfo {
        id: String::new(),
        name: "status.default_output".to_string(),
    }];

    // The apartment is scoped to this call. Enumeration happens on an operator
    // action rather than per frame, so the cost of entering and leaving one is
    // paid a handful of times per session.
    let _apartment = Apartment::new(true);

    unsafe {
        let mut enumerator: ComPtr<IMMDeviceEnumerator> = ComPtr::null();
        let hr = CoCreateInstance(
            &CLSID_MMDeviceEnumerator,
            std::ptr::null_mut(),
            CLSCTX_ALL,
            &IID_IMMDeviceEnumerator,
            enumerator.out() as *mut *mut std::ffi::c_void,
        );
        if hr != S_OK || enumerator.is_null() {
            crate::log_warn!("audio", "cannot create the device enumerator, 0x{:08X}", hr as u32);
            return out;
        }
        let table = &*(*enumerator.as_raw()).vtbl;

        let mut collection: ComPtr<IMMDeviceCollection> = ComPtr::null();
        let hr = (table.EnumAudioEndpoints)(
            enumerator.as_raw(),
            E_RENDER,
            DEVICE_STATE_ACTIVE,
            collection.out(),
        );
        if hr != S_OK || collection.is_null() {
            crate::log_warn!("audio", "cannot enumerate endpoints, 0x{:08X}", hr as u32);
            return out;
        }
        let ctable = &*(*collection.as_raw()).vtbl;

        let mut count: u32 = 0;
        if (ctable.GetCount)(collection.as_raw(), &mut count) != S_OK {
            return out;
        }

        for index in 0..count {
            let mut device: ComPtr<IMMDevice> = ComPtr::null();
            if (ctable.Item)(collection.as_raw(), index, device.out()) != S_OK {
                continue;
            }
            if let Some(info) = read_device(&device) {
                out.push(info);
            }
        }
    }

    crate::log_info!("audio", "{} render endpoints found", out.len() - 1);
    out
}

/// Reads the identifier and the friendly name of one endpoint.
unsafe fn read_device(device: &ComPtr<IMMDevice>) -> Option<DeviceInfo> {
    let table = &*(*device.as_raw()).vtbl;

    let mut raw_id: *mut u16 = std::ptr::null_mut();
    if (table.GetId)(device.as_raw(), &mut raw_id) != S_OK || raw_id.is_null() {
        return None;
    }
    let id = string_from_wide_ptr(raw_id);
    CoTaskMemFree(raw_id as *mut std::ffi::c_void);

    let mut store: ComPtr<IPropertyStore> = ComPtr::null();
    if (table.OpenPropertyStore)(device.as_raw(), STGM_READ, store.out()) != S_OK
        || store.is_null()
    {
        // The identifier alone is enough to open it, so a missing name is not a
        // reason to hide the endpoint.
        return Some(DeviceInfo { name: id.clone(), id });
    }
    let stable = &*(*store.as_raw()).vtbl;

    let mut value = PROPVARIANT::default();
    let name = if (stable.GetValue)(store.as_raw(), &PKEY_Device_FriendlyName, &mut value) == S_OK
        && value.vt == VT_LPWSTR
    {
        let text = string_from_wide_ptr(value.as_wide_ptr());
        PropVariantClear(&mut value);
        text
    } else {
        PropVariantClear(&mut value);
        id.clone()
    };

    Some(DeviceInfo { id, name })
}

/// Opens the endpoint an identifier names, or the default when it is empty.
pub(crate) unsafe fn open_device(id: &str) -> crate::core::Result<ComPtr<IMMDevice>> {
    use crate::core::Error;

    let mut enumerator: ComPtr<IMMDeviceEnumerator> = ComPtr::null();
    let hr = CoCreateInstance(
        &CLSID_MMDeviceEnumerator,
        std::ptr::null_mut(),
        CLSCTX_ALL,
        &IID_IMMDeviceEnumerator,
        enumerator.out() as *mut *mut std::ffi::c_void,
    );
    if hr != S_OK || enumerator.is_null() {
        return Err(Error::with_code(
            crate::core::error::Category::Audio,
            "cannot create the device enumerator",
            hr as i64,
        ));
    }
    let table = &*(*enumerator.as_raw()).vtbl;

    let mut device: ComPtr<IMMDevice> = ComPtr::null();
    let hr = if id.is_empty() {
        (table.GetDefaultAudioEndpoint)(enumerator.as_raw(), E_RENDER, E_CONSOLE, device.out())
    } else {
        let wide_id = wide(id);
        (table.GetDevice)(enumerator.as_raw(), wide_id.as_ptr(), device.out())
    };

    if hr != S_OK || device.is_null() {
        return Err(Error::with_code(
            crate::core::error::Category::Audio,
            if id.is_empty() {
                "no default output endpoint".to_string()
            } else {
                format!("cannot open the endpoint {}", id)
            },
            hr as i64,
        ));
    }
    Ok(device)
}