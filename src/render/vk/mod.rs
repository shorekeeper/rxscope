//! Vulkan bindings.
//!
//! Hand written rather than generated, because the surface actually used is
//! small and entirely core: no extension chain is ever built, so the generic
//! machinery a generator produces to support `pNext` would be dead code that
//! still has to be understood.
//!
//! Two rules govern everything in this module. Declarations follow the
//! specification headers verbatim, so an audit is a textual comparison rather
//! than a reconstruction. And nothing here interprets: a value the driver
//! returns is carried through as an integer, and the naming helpers below map
//! it to text only for the log.

pub mod loader;
pub mod result;
pub mod types;

#[cfg(test)]
mod layout;

pub use loader::{DeviceFns, Entry, InstanceFns};
pub use result::{check, vk_err, NO_ALLOCATOR};
pub use types::*;

use std::ffi::{c_char, CStr};

/// Reads a fixed size name field the driver filled in.
///
/// The array is null terminated within its bounds; a driver that fills it
/// completely without a terminator would be malformed, so the length is capped
/// at the array itself rather than trusted from the contents.
pub fn name_from_array(raw: &[c_char]) -> String {
    let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    let bytes = unsafe { std::slice::from_raw_parts(raw.as_ptr() as *const u8, end) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// Compares a driver supplied name against a null terminated literal.
pub fn name_matches(raw: &[c_char], wanted: &[u8]) -> bool {
    let expect = match wanted.split_last() {
        Some((0, head)) => head,
        _ => wanted,
    };
    let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    if end != expect.len() {
        return false;
    }
    let bytes = unsafe { std::slice::from_raw_parts(raw.as_ptr() as *const u8, end) };
    bytes == expect
}

/// Reads a null terminated string the driver owns.
///
/// Safety: the pointer must be null or point at a terminated string that lives
/// for the duration of the call.
pub unsafe fn string_from_ptr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    CStr::from_ptr(p).to_string_lossy().into_owned()
}

/// Result code as text.
///
/// Unknown values are printed numerically rather than mapped to a placeholder,
/// because a code this build does not know is exactly the case where the number
/// itself is the only useful information.
pub fn result_name(r: VkResult) -> String {
    let text = match r {
        VK_SUCCESS => "VK_SUCCESS",
        VK_NOT_READY => "VK_NOT_READY",
        VK_TIMEOUT => "VK_TIMEOUT",
        VK_EVENT_SET => "VK_EVENT_SET",
        VK_EVENT_RESET => "VK_EVENT_RESET",
        VK_INCOMPLETE => "VK_INCOMPLETE",
        VK_ERROR_OUT_OF_HOST_MEMORY => "VK_ERROR_OUT_OF_HOST_MEMORY",
        VK_ERROR_OUT_OF_DEVICE_MEMORY => "VK_ERROR_OUT_OF_DEVICE_MEMORY",
        VK_ERROR_INITIALIZATION_FAILED => "VK_ERROR_INITIALIZATION_FAILED",
        VK_ERROR_DEVICE_LOST => "VK_ERROR_DEVICE_LOST",
        VK_ERROR_MEMORY_MAP_FAILED => "VK_ERROR_MEMORY_MAP_FAILED",
        VK_ERROR_LAYER_NOT_PRESENT => "VK_ERROR_LAYER_NOT_PRESENT",
        VK_ERROR_EXTENSION_NOT_PRESENT => "VK_ERROR_EXTENSION_NOT_PRESENT",
        VK_ERROR_FEATURE_NOT_PRESENT => "VK_ERROR_FEATURE_NOT_PRESENT",
        VK_ERROR_INCOMPATIBLE_DRIVER => "VK_ERROR_INCOMPATIBLE_DRIVER",
        VK_ERROR_TOO_MANY_OBJECTS => "VK_ERROR_TOO_MANY_OBJECTS",
        VK_ERROR_FORMAT_NOT_SUPPORTED => "VK_ERROR_FORMAT_NOT_SUPPORTED",
        VK_ERROR_FRAGMENTED_POOL => "VK_ERROR_FRAGMENTED_POOL",
        VK_ERROR_UNKNOWN => "VK_ERROR_UNKNOWN",
        VK_ERROR_OUT_OF_POOL_MEMORY => "VK_ERROR_OUT_OF_POOL_MEMORY",
        VK_ERROR_SURFACE_LOST_KHR => "VK_ERROR_SURFACE_LOST_KHR",
        VK_ERROR_NATIVE_WINDOW_IN_USE_KHR => "VK_ERROR_NATIVE_WINDOW_IN_USE_KHR",
        VK_SUBOPTIMAL_KHR => "VK_SUBOPTIMAL_KHR",
        VK_ERROR_OUT_OF_DATE_KHR => "VK_ERROR_OUT_OF_DATE_KHR",
        VK_ERROR_INCOMPATIBLE_DISPLAY_KHR => "VK_ERROR_INCOMPATIBLE_DISPLAY_KHR",
        VK_ERROR_VALIDATION_FAILED_EXT => "VK_ERROR_VALIDATION_FAILED_EXT",
        other => return format!("VkResult {}", other),
    };
    text.to_string()
}

pub fn physical_device_type_name(t: VkPhysicalDeviceType) -> &'static str {
    match t {
        VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU => "integrated",
        VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU => "discrete",
        VK_PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU => "virtual",
        VK_PHYSICAL_DEVICE_TYPE_CPU => "cpu",
        _ => "other",
    }
}

/// Format as text, covering the values this renderer can select.
///
/// A swapchain may report a format outside that set, which is why the fallback
/// prints the number: the picture is wrong in that case and the number is what
/// identifies the format that was chosen.
pub fn format_name(f: VkFormat) -> String {
    let text = match f {
        VK_FORMAT_UNDEFINED => "UNDEFINED",
        VK_FORMAT_R8_UNORM => "R8_UNORM",
        VK_FORMAT_R8G8B8A8_UNORM => "R8G8B8A8_UNORM",
        VK_FORMAT_R8G8B8A8_SRGB => "R8G8B8A8_SRGB",
        VK_FORMAT_B8G8R8A8_UNORM => "B8G8R8A8_UNORM",
        VK_FORMAT_B8G8R8A8_SRGB => "B8G8R8A8_SRGB",
        VK_FORMAT_R32_UINT => "R32_UINT",
        VK_FORMAT_R32_SFLOAT => "R32_SFLOAT",
        VK_FORMAT_R32G32_SFLOAT => "R32G32_SFLOAT",
        other => return format!("VkFormat {}", other),
    };
    text.to_string()
}

pub fn present_mode_name(m: VkPresentModeKHR) -> String {
    let text = match m {
        VK_PRESENT_MODE_IMMEDIATE_KHR => "immediate",
        VK_PRESENT_MODE_MAILBOX_KHR => "mailbox",
        VK_PRESENT_MODE_FIFO_KHR => "fifo",
        VK_PRESENT_MODE_FIFO_RELAXED_KHR => "fifo relaxed",
        other => return format!("VkPresentModeKHR {}", other),
    };
    text.to_string()
}

/// Reads an enumeration that reports its own count.
///
/// Every such call in the API is two calls: one with a null destination to
/// learn the count and one to fill it. The pattern is written once here because
/// getting it wrong in one place produces a truncated list rather than an
/// error, and a truncated device list is a device that silently cannot be
/// selected.
///
/// The destination is grown by capacity and its length is set afterwards rather
/// than filled with a default value first. Two reasons: the default write would
/// be discarded immediately by the driver, and one of the enumerated types is a
/// dispatchable handle, which is a raw pointer and therefore has no `Default`
/// implementation that this crate is permitted to supply.
///
/// An incomplete result is not a failure. It means the count grew between the
/// two calls, and what was written is valid; the next rescan sees the rest.
///
/// Safety of the length assignment rests on the enumeration contract of the
/// API: the callee writes exactly as many elements as it reports back, and the
/// reported count cannot exceed the capacity it was given. `T` is constrained
/// to `Copy` so no element carries a destructor that would run on uninitialized
/// storage.
pub fn enumerate<T, F>(mut call: F) -> std::result::Result<Vec<T>, VkResult>
where
    T: Copy,
    F: FnMut(*mut u32, *mut T) -> VkResult,
{
    let mut count: u32 = 0;
    let r = call(&mut count, std::ptr::null_mut());
    if r < 0 {
        return Err(r);
    }
    if count == 0 {
        return Ok(Vec::new());
    }

    let mut items: Vec<T> = Vec::with_capacity(count as usize);
    let capacity = count;
    let r = call(&mut count, items.as_mut_ptr());
    if r < 0 {
        return Err(r);
    }

    let written = count.min(capacity) as usize;
    unsafe { items.set_len(written) };
    Ok(items)
}