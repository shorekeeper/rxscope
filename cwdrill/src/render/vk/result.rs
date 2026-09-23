//! Result handling.
//!
//! Held apart from the declarations so the files rewritten later can depend on
//! it without depending on the renderer facade, which is itself one of them.
//!
//! A code at or above nought is a success. The specification splits the space
//! that way and uses the positive side for successes that carry information,
//! such as an enumeration truncated because the count grew or a swapchain that
//! still presents but no longer matches the surface. Treating those as failures
//! would abandon a frame that is perfectly presentable.

use crate::core::{Error, Result};

use super::types::{VkAllocationCallbacks, VkResult};
use super::result_name;

/// Allocation callbacks are never supplied. Named rather than written inline at
/// every call site, because a null of the wrong type is the one mistake the
/// compiler cannot catch here.
pub const NO_ALLOCATOR: *const VkAllocationCallbacks = std::ptr::null();

/// Builds an error carrying the operation and the code.
///
/// The numeric code is kept beside the name so a report from the field can be
/// matched against a driver release note even when this build has no name for
/// the value.
pub fn vk_err(op: &str, r: VkResult) -> Error {
    Error::with_code(
        crate::core::error::Category::Vulkan,
        format!("{} failed: {}", op, result_name(r)),
        r as i64,
    )
}

/// Turns a code into a result.
pub fn check(op: &str, r: VkResult) -> Result<()> {
    if r >= 0 {
        Ok(())
    } else {
        Err(vk_err(op, r))
    }
}