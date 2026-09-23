//! Win32 surface and the capability queries the swapchain plan needs.
//!
//! The surface is an instance level object, so every query below goes through
//! the instance table rather than the device one.

use crate::core::Result;
use crate::render::instance::Instance;
use crate::render::vk::*;

pub struct Surface {
    pub handle: VkSurfaceKHR,
}

impl Surface {
    pub fn new(
        instance: &Instance,
        hwnd: *mut std::ffi::c_void,
        hinstance: *mut std::ffi::c_void,
    ) -> Result<Surface> {
        let info = VkWin32SurfaceCreateInfoKHR {
            hinstance,
            hwnd,
            ..Default::default()
        };

        let mut handle: VkSurfaceKHR = VK_NULL_HANDLE;
        check("vkCreateWin32SurfaceKHR", unsafe {
            (instance.fns.create_win32_surface_khr)(
                instance.handle,
                &info,
                NO_ALLOCATOR,
                &mut handle,
            )
        })?;
        Ok(Surface { handle })
    }

    pub fn capabilities(
        &self,
        instance: &Instance,
        physical: VkPhysicalDevice,
    ) -> Result<VkSurfaceCapabilitiesKHR> {
        let mut caps = VkSurfaceCapabilitiesKHR::default();
        check("vkGetPhysicalDeviceSurfaceCapabilitiesKHR", unsafe {
            (instance.fns.get_physical_device_surface_capabilities_khr)(
                physical,
                self.handle,
                &mut caps,
            )
        })?;
        Ok(caps)
    }

    pub fn formats(
        &self,
        instance: &Instance,
        physical: VkPhysicalDevice,
    ) -> Result<Vec<VkSurfaceFormatKHR>> {
        enumerate(|count, data| unsafe {
            (instance.fns.get_physical_device_surface_formats_khr)(
                physical,
                self.handle,
                count,
                data,
            )
        })
        .map_err(|r| vk_err("vkGetPhysicalDeviceSurfaceFormatsKHR", r))
    }

    pub fn present_modes(
        &self,
        instance: &Instance,
        physical: VkPhysicalDevice,
    ) -> Result<Vec<VkPresentModeKHR>> {
        enumerate(|count, data| unsafe {
            (instance.fns.get_physical_device_surface_present_modes_khr)(
                physical,
                self.handle,
                count,
                data,
            )
        })
        .map_err(|r| vk_err("vkGetPhysicalDeviceSurfacePresentModesKHR", r))
    }

    /// True when the queue family can present to this surface.
    ///
    /// A failure is reported as unsupported rather than propagated: the caller
    /// is scoring candidates, and a family that cannot be queried is a family
    /// that cannot be relied on.
    pub fn supports(
        &self,
        instance: &Instance,
        physical: VkPhysicalDevice,
        queue_family: u32,
    ) -> bool {
        let mut supported: VkBool32 = VK_FALSE;
        let r = unsafe {
            (instance.fns.get_physical_device_surface_support_khr)(
                physical,
                queue_family,
                self.handle,
                &mut supported,
            )
        };
        r >= 0 && supported == VK_TRUE
    }

    pub fn destroy(&mut self, instance: &Instance) {
        if self.handle != VK_NULL_HANDLE {
            unsafe {
                (instance.fns.destroy_surface_khr)(instance.handle, self.handle, NO_ALLOCATOR);
            }
            self.handle = VK_NULL_HANDLE;
        }
    }
}