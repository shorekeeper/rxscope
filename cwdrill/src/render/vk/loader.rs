//! Entry point resolution.
//!
//! Three tables, matching the three dispatch levels of the API.
//!
//! `Entry` holds the four functions the loader answers with a null instance,
//! and is obtained from the loader library itself. `InstanceFns` holds
//! everything that takes an instance or a physical device. `DeviceFns` holds
//! everything that takes a device, a queue or a command buffer.
//!
//! Device level functions are resolved through `vkGetDeviceProcAddr` rather
//! than `vkGetInstanceProcAddr`. The instance form is permitted and returns a
//! trampoline that reads the dispatch table on every call to decide which
//! driver to forward to; the device form returns the driver entry point
//! directly. On the command recording path that indirection is paid a dozen
//! times per draw command.
//!
//! The library is never unloaded. A driver may keep thread local state and
//! background threads alive behind the loader, and unloading it at process exit
//! risks tearing that down while it is still in use, for no benefit: the
//! process is ending anyway.

use std::ffi::{c_char, c_void};

use crate::core::{Error, Result};
use crate::platform::win32::ffi::{GetProcAddress, LoadLibraryW, HMODULE};
use crate::platform::win32::wide;

use super::types::*;

pub type PFN_vkGetInstanceProcAddr =
    unsafe extern "system" fn(VkInstance, *const c_char) -> PFN_vkVoidFunction;
pub type PFN_vkGetDeviceProcAddr =
    unsafe extern "system" fn(VkDevice, *const c_char) -> PFN_vkVoidFunction;

/// Resolves a name and transmutes it into the declared signature.
///
/// A missing name is an error rather than an option: every entry point loaded
/// through this macro belongs either to the core API or to an extension whose
/// presence was checked beforehand, so absence means the loader and the driver
/// disagree about what was enabled, and continuing would call through a null
/// pointer.
macro_rules! load {
    ($get:expr, $handle:expr, $name:literal) => {{
        let raw = $get($handle, concat!($name, "\0").as_ptr() as *const c_char);
        if raw.is_null() {
            return Err(Error::vulkan(concat!("entry point ", $name, " is missing")));
        }
        std::mem::transmute(raw)
    }};
}

/// Resolves a name that may legitimately be absent.
macro_rules! load_optional {
    ($get:expr, $handle:expr, $name:literal) => {{
        let raw = $get($handle, concat!($name, "\0").as_ptr() as *const c_char);
        if raw.is_null() {
            None
        } else {
            Some(std::mem::transmute(raw))
        }
    }};
}

pub struct Entry {
    /// Kept so instance level resolution has a function to call.
    pub get_instance_proc_addr: PFN_vkGetInstanceProcAddr,
    pub create_instance: unsafe extern "system" fn(
        *const VkInstanceCreateInfo,
        *const VkAllocationCallbacks,
        *mut VkInstance,
    ) -> VkResult,
    pub enumerate_instance_extension_properties: unsafe extern "system" fn(
        *const c_char,
        *mut u32,
        *mut VkExtensionProperties,
    ) -> VkResult,
    pub enumerate_instance_layer_properties:
        unsafe extern "system" fn(*mut u32, *mut VkLayerProperties) -> VkResult,
    /// Added in version 1.1, so a loader that predates it has no such symbol
    /// and the absence itself reports the version.
    pub enumerate_instance_version: Option<unsafe extern "system" fn(*mut u32) -> VkResult>,
    library: HMODULE,
}

impl Entry {
    pub fn load() -> Result<Entry> {
        let name = wide("vulkan-1.dll");
        let library = unsafe { LoadLibraryW(name.as_ptr()) };
        if library.is_null() {
            return Err(Error::vulkan(
                "cannot load vulkan-1.dll, no Vulkan capable driver is installed",
            ));
        }

        let raw = unsafe {
            GetProcAddress(library, b"vkGetInstanceProcAddr\0".as_ptr() as *const i8)
        };
        if raw.is_null() {
            return Err(Error::vulkan("vulkan-1.dll exports no vkGetInstanceProcAddr"));
        }

        unsafe {
            let get: PFN_vkGetInstanceProcAddr = std::mem::transmute(raw);
            let null = std::ptr::null_mut();
            Ok(Entry {
                get_instance_proc_addr: get,
                create_instance: load!(get, null, "vkCreateInstance"),
                enumerate_instance_extension_properties: load!(
                    get,
                    null,
                    "vkEnumerateInstanceExtensionProperties"
                ),
                enumerate_instance_layer_properties: load!(
                    get,
                    null,
                    "vkEnumerateInstanceLayerProperties"
                ),
                enumerate_instance_version: load_optional!(
                    get,
                    null,
                    "vkEnumerateInstanceVersion"
                ),
                library,
            })
        }
    }

    pub fn library(&self) -> HMODULE {
        self.library
    }
}

pub struct InstanceFns {
    pub destroy_instance:
        unsafe extern "system" fn(VkInstance, *const VkAllocationCallbacks),
    pub enumerate_physical_devices:
        unsafe extern "system" fn(VkInstance, *mut u32, *mut VkPhysicalDevice) -> VkResult,
    pub get_physical_device_properties:
        unsafe extern "system" fn(VkPhysicalDevice, *mut VkPhysicalDeviceProperties),
    pub get_physical_device_memory_properties:
        unsafe extern "system" fn(VkPhysicalDevice, *mut VkPhysicalDeviceMemoryProperties),
    pub get_physical_device_queue_family_properties: unsafe extern "system" fn(
        VkPhysicalDevice,
        *mut u32,
        *mut VkQueueFamilyProperties,
    ),
    pub enumerate_device_extension_properties: unsafe extern "system" fn(
        VkPhysicalDevice,
        *const c_char,
        *mut u32,
        *mut VkExtensionProperties,
    ) -> VkResult,
    pub create_device: unsafe extern "system" fn(
        VkPhysicalDevice,
        *const VkDeviceCreateInfo,
        *const VkAllocationCallbacks,
        *mut VkDevice,
    ) -> VkResult,
    pub get_device_proc_addr: PFN_vkGetDeviceProcAddr,

    pub destroy_surface_khr:
        unsafe extern "system" fn(VkInstance, VkSurfaceKHR, *const VkAllocationCallbacks),
    pub get_physical_device_surface_support_khr: unsafe extern "system" fn(
        VkPhysicalDevice,
        u32,
        VkSurfaceKHR,
        *mut VkBool32,
    ) -> VkResult,
    pub get_physical_device_surface_capabilities_khr: unsafe extern "system" fn(
        VkPhysicalDevice,
        VkSurfaceKHR,
        *mut VkSurfaceCapabilitiesKHR,
    ) -> VkResult,
    pub get_physical_device_surface_formats_khr: unsafe extern "system" fn(
        VkPhysicalDevice,
        VkSurfaceKHR,
        *mut u32,
        *mut VkSurfaceFormatKHR,
    ) -> VkResult,
    pub get_physical_device_surface_present_modes_khr: unsafe extern "system" fn(
        VkPhysicalDevice,
        VkSurfaceKHR,
        *mut u32,
        *mut VkPresentModeKHR,
    ) -> VkResult,
    pub create_win32_surface_khr: unsafe extern "system" fn(
        VkInstance,
        *const VkWin32SurfaceCreateInfoKHR,
        *const VkAllocationCallbacks,
        *mut VkSurfaceKHR,
    ) -> VkResult,

    /// Present only when the debug extension was enabled.
    pub create_debug_utils_messenger_ext: Option<
        unsafe extern "system" fn(
            VkInstance,
            *const VkDebugUtilsMessengerCreateInfoEXT,
            *const VkAllocationCallbacks,
            *mut VkDebugUtilsMessengerEXT,
        ) -> VkResult,
    >,
    pub destroy_debug_utils_messenger_ext: Option<
        unsafe extern "system" fn(
            VkInstance,
            VkDebugUtilsMessengerEXT,
            *const VkAllocationCallbacks,
        ),
    >,
}

impl InstanceFns {
    /// Resolves the instance level table.
    ///
    /// Safety: the handle must be a live instance created through the same
    /// entry, and the debug flag must reflect whether the extension was named
    /// in the create info.
    pub unsafe fn load(entry: &Entry, instance: VkInstance, debug: bool) -> Result<InstanceFns> {
        let get = entry.get_instance_proc_addr;
        Ok(InstanceFns {
            destroy_instance: load!(get, instance, "vkDestroyInstance"),
            enumerate_physical_devices: load!(get, instance, "vkEnumeratePhysicalDevices"),
            get_physical_device_properties: load!(
                get,
                instance,
                "vkGetPhysicalDeviceProperties"
            ),
            get_physical_device_memory_properties: load!(
                get,
                instance,
                "vkGetPhysicalDeviceMemoryProperties"
            ),
            get_physical_device_queue_family_properties: load!(
                get,
                instance,
                "vkGetPhysicalDeviceQueueFamilyProperties"
            ),
            enumerate_device_extension_properties: load!(
                get,
                instance,
                "vkEnumerateDeviceExtensionProperties"
            ),
            create_device: load!(get, instance, "vkCreateDevice"),
            get_device_proc_addr: load!(get, instance, "vkGetDeviceProcAddr"),

            destroy_surface_khr: load!(get, instance, "vkDestroySurfaceKHR"),
            get_physical_device_surface_support_khr: load!(
                get,
                instance,
                "vkGetPhysicalDeviceSurfaceSupportKHR"
            ),
            get_physical_device_surface_capabilities_khr: load!(
                get,
                instance,
                "vkGetPhysicalDeviceSurfaceCapabilitiesKHR"
            ),
            get_physical_device_surface_formats_khr: load!(
                get,
                instance,
                "vkGetPhysicalDeviceSurfaceFormatsKHR"
            ),
            get_physical_device_surface_present_modes_khr: load!(
                get,
                instance,
                "vkGetPhysicalDeviceSurfacePresentModesKHR"
            ),
            create_win32_surface_khr: load!(get, instance, "vkCreateWin32SurfaceKHR"),

            create_debug_utils_messenger_ext: if debug {
                load_optional!(get, instance, "vkCreateDebugUtilsMessengerEXT")
            } else {
                None
            },
            destroy_debug_utils_messenger_ext: if debug {
                load_optional!(get, instance, "vkDestroyDebugUtilsMessengerEXT")
            } else {
                None
            },
        })
    }
}

pub struct DeviceFns {
    pub destroy_device: unsafe extern "system" fn(VkDevice, *const VkAllocationCallbacks),
    pub get_device_queue: unsafe extern "system" fn(VkDevice, u32, u32, *mut VkQueue),
    pub device_wait_idle: unsafe extern "system" fn(VkDevice) -> VkResult,
    pub queue_submit:
        unsafe extern "system" fn(VkQueue, u32, *const VkSubmitInfo, VkFence) -> VkResult,

    pub create_command_pool: unsafe extern "system" fn(
        VkDevice,
        *const VkCommandPoolCreateInfo,
        *const VkAllocationCallbacks,
        *mut VkCommandPool,
    ) -> VkResult,
    pub destroy_command_pool:
        unsafe extern "system" fn(VkDevice, VkCommandPool, *const VkAllocationCallbacks),
    pub reset_command_pool:
        unsafe extern "system" fn(VkDevice, VkCommandPool, VkCommandPoolResetFlags) -> VkResult,
    pub allocate_command_buffers: unsafe extern "system" fn(
        VkDevice,
        *const VkCommandBufferAllocateInfo,
        *mut VkCommandBuffer,
    ) -> VkResult,
    pub free_command_buffers:
        unsafe extern "system" fn(VkDevice, VkCommandPool, u32, *const VkCommandBuffer),
    pub begin_command_buffer:
        unsafe extern "system" fn(VkCommandBuffer, *const VkCommandBufferBeginInfo) -> VkResult,
    pub end_command_buffer: unsafe extern "system" fn(VkCommandBuffer) -> VkResult,

    pub create_fence: unsafe extern "system" fn(
        VkDevice,
        *const VkFenceCreateInfo,
        *const VkAllocationCallbacks,
        *mut VkFence,
    ) -> VkResult,
    pub destroy_fence:
        unsafe extern "system" fn(VkDevice, VkFence, *const VkAllocationCallbacks),
    pub reset_fences: unsafe extern "system" fn(VkDevice, u32, *const VkFence) -> VkResult,
    pub wait_for_fences:
        unsafe extern "system" fn(VkDevice, u32, *const VkFence, VkBool32, u64) -> VkResult,
    pub create_semaphore: unsafe extern "system" fn(
        VkDevice,
        *const VkSemaphoreCreateInfo,
        *const VkAllocationCallbacks,
        *mut VkSemaphore,
    ) -> VkResult,
    pub destroy_semaphore:
        unsafe extern "system" fn(VkDevice, VkSemaphore, *const VkAllocationCallbacks),
    pub create_query_pool: unsafe extern "system" fn(
        VkDevice,
        *const VkQueryPoolCreateInfo,
        *const VkAllocationCallbacks,
        *mut VkQueryPool,
    ) -> VkResult,
    pub destroy_query_pool:
        unsafe extern "system" fn(VkDevice, VkQueryPool, *const VkAllocationCallbacks),
    pub get_query_pool_results: unsafe extern "system" fn(
        VkDevice,
        VkQueryPool,
        u32,
        u32,
        usize,
        *mut c_void,
        VkDeviceSize,
        VkQueryResultFlags,
    ) -> VkResult,
    pub cmd_reset_query_pool:
        unsafe extern "system" fn(VkCommandBuffer, VkQueryPool, u32, u32),
    pub cmd_write_timestamp:
        unsafe extern "system" fn(VkCommandBuffer, VkPipelineStageFlags, VkQueryPool, u32),

    pub create_buffer: unsafe extern "system" fn(
        VkDevice,
        *const VkBufferCreateInfo,
        *const VkAllocationCallbacks,
        *mut VkBuffer,
    ) -> VkResult,
    pub destroy_buffer:
        unsafe extern "system" fn(VkDevice, VkBuffer, *const VkAllocationCallbacks),
    pub get_buffer_memory_requirements:
        unsafe extern "system" fn(VkDevice, VkBuffer, *mut VkMemoryRequirements),
    pub bind_buffer_memory:
        unsafe extern "system" fn(VkDevice, VkBuffer, VkDeviceMemory, VkDeviceSize) -> VkResult,

    pub allocate_memory: unsafe extern "system" fn(
        VkDevice,
        *const VkMemoryAllocateInfo,
        *const VkAllocationCallbacks,
        *mut VkDeviceMemory,
    ) -> VkResult,
    pub free_memory:
        unsafe extern "system" fn(VkDevice, VkDeviceMemory, *const VkAllocationCallbacks),
    pub map_memory: unsafe extern "system" fn(
        VkDevice,
        VkDeviceMemory,
        VkDeviceSize,
        VkDeviceSize,
        VkMemoryMapFlags,
        *mut *mut c_void,
    ) -> VkResult,
    pub unmap_memory: unsafe extern "system" fn(VkDevice, VkDeviceMemory),
    pub flush_mapped_memory_ranges:
        unsafe extern "system" fn(VkDevice, u32, *const VkMappedMemoryRange) -> VkResult,

    pub create_image: unsafe extern "system" fn(
        VkDevice,
        *const VkImageCreateInfo,
        *const VkAllocationCallbacks,
        *mut VkImage,
    ) -> VkResult,
    pub destroy_image: unsafe extern "system" fn(VkDevice, VkImage, *const VkAllocationCallbacks),
    pub get_image_memory_requirements:
        unsafe extern "system" fn(VkDevice, VkImage, *mut VkMemoryRequirements),
    pub bind_image_memory:
        unsafe extern "system" fn(VkDevice, VkImage, VkDeviceMemory, VkDeviceSize) -> VkResult,
    pub create_image_view: unsafe extern "system" fn(
        VkDevice,
        *const VkImageViewCreateInfo,
        *const VkAllocationCallbacks,
        *mut VkImageView,
    ) -> VkResult,
    pub destroy_image_view:
        unsafe extern "system" fn(VkDevice, VkImageView, *const VkAllocationCallbacks),
    pub create_sampler: unsafe extern "system" fn(
        VkDevice,
        *const VkSamplerCreateInfo,
        *const VkAllocationCallbacks,
        *mut VkSampler,
    ) -> VkResult,
    pub destroy_sampler:
        unsafe extern "system" fn(VkDevice, VkSampler, *const VkAllocationCallbacks),

    pub create_descriptor_set_layout: unsafe extern "system" fn(
        VkDevice,
        *const VkDescriptorSetLayoutCreateInfo,
        *const VkAllocationCallbacks,
        *mut VkDescriptorSetLayout,
    ) -> VkResult,
    pub destroy_descriptor_set_layout: unsafe extern "system" fn(
        VkDevice,
        VkDescriptorSetLayout,
        *const VkAllocationCallbacks,
    ),
    pub create_descriptor_pool: unsafe extern "system" fn(
        VkDevice,
        *const VkDescriptorPoolCreateInfo,
        *const VkAllocationCallbacks,
        *mut VkDescriptorPool,
    ) -> VkResult,
    pub destroy_descriptor_pool:
        unsafe extern "system" fn(VkDevice, VkDescriptorPool, *const VkAllocationCallbacks),
    pub allocate_descriptor_sets: unsafe extern "system" fn(
        VkDevice,
        *const VkDescriptorSetAllocateInfo,
        *mut VkDescriptorSet,
    ) -> VkResult,
    pub update_descriptor_sets: unsafe extern "system" fn(
        VkDevice,
        u32,
        *const VkWriteDescriptorSet,
        u32,
        *const c_void,
    ),

    pub create_pipeline_layout: unsafe extern "system" fn(
        VkDevice,
        *const VkPipelineLayoutCreateInfo,
        *const VkAllocationCallbacks,
        *mut VkPipelineLayout,
    ) -> VkResult,
    pub destroy_pipeline_layout:
        unsafe extern "system" fn(VkDevice, VkPipelineLayout, *const VkAllocationCallbacks),
    pub create_shader_module: unsafe extern "system" fn(
        VkDevice,
        *const VkShaderModuleCreateInfo,
        *const VkAllocationCallbacks,
        *mut VkShaderModule,
    ) -> VkResult,
    pub destroy_shader_module:
        unsafe extern "system" fn(VkDevice, VkShaderModule, *const VkAllocationCallbacks),
    pub create_graphics_pipelines: unsafe extern "system" fn(
        VkDevice,
        VkPipelineCache,
        u32,
        *const VkGraphicsPipelineCreateInfo,
        *const VkAllocationCallbacks,
        *mut VkPipeline,
    ) -> VkResult,
    pub destroy_pipeline:
        unsafe extern "system" fn(VkDevice, VkPipeline, *const VkAllocationCallbacks),

    pub create_render_pass: unsafe extern "system" fn(
        VkDevice,
        *const VkRenderPassCreateInfo,
        *const VkAllocationCallbacks,
        *mut VkRenderPass,
    ) -> VkResult,
    pub destroy_render_pass:
        unsafe extern "system" fn(VkDevice, VkRenderPass, *const VkAllocationCallbacks),
    pub create_framebuffer: unsafe extern "system" fn(
        VkDevice,
        *const VkFramebufferCreateInfo,
        *const VkAllocationCallbacks,
        *mut VkFramebuffer,
    ) -> VkResult,
    pub destroy_framebuffer:
        unsafe extern "system" fn(VkDevice, VkFramebuffer, *const VkAllocationCallbacks),

    pub cmd_begin_render_pass: unsafe extern "system" fn(
        VkCommandBuffer,
        *const VkRenderPassBeginInfo,
        VkSubpassContents,
    ),
    pub cmd_end_render_pass: unsafe extern "system" fn(VkCommandBuffer),
    pub cmd_bind_pipeline:
        unsafe extern "system" fn(VkCommandBuffer, VkPipelineBindPoint, VkPipeline),
    pub cmd_set_viewport:
        unsafe extern "system" fn(VkCommandBuffer, u32, u32, *const VkViewport),
    pub cmd_set_scissor: unsafe extern "system" fn(VkCommandBuffer, u32, u32, *const VkRect2D),
    pub cmd_push_constants: unsafe extern "system" fn(
        VkCommandBuffer,
        VkPipelineLayout,
        VkShaderStageFlags,
        u32,
        u32,
        *const c_void,
    ),
    pub cmd_bind_vertex_buffers: unsafe extern "system" fn(
        VkCommandBuffer,
        u32,
        u32,
        *const VkBuffer,
        *const VkDeviceSize,
    ),
    pub cmd_bind_index_buffer:
        unsafe extern "system" fn(VkCommandBuffer, VkBuffer, VkDeviceSize, VkIndexType),
    pub cmd_bind_descriptor_sets: unsafe extern "system" fn(
        VkCommandBuffer,
        VkPipelineBindPoint,
        VkPipelineLayout,
        u32,
        u32,
        *const VkDescriptorSet,
        u32,
        *const u32,
    ),
    pub cmd_draw_indexed:
        unsafe extern "system" fn(VkCommandBuffer, u32, u32, u32, i32, u32),
    pub cmd_pipeline_barrier: unsafe extern "system" fn(
        VkCommandBuffer,
        VkPipelineStageFlags,
        VkPipelineStageFlags,
        VkDependencyFlags,
        u32,
        *const c_void,
        u32,
        *const c_void,
        u32,
        *const VkImageMemoryBarrier,
    ),
    pub cmd_copy_buffer_to_image: unsafe extern "system" fn(
        VkCommandBuffer,
        VkBuffer,
        VkImage,
        VkImageLayout,
        u32,
        *const VkBufferImageCopy,
    ),

    pub cmd_clear_color_image: unsafe extern "system" fn(
        VkCommandBuffer,
        VkImage,
        VkImageLayout,
        *const VkClearColorValue,
        u32,
        *const VkImageSubresourceRange,
    ),

    pub create_swapchain_khr: unsafe extern "system" fn(
        VkDevice,
        *const VkSwapchainCreateInfoKHR,
        *const VkAllocationCallbacks,
        *mut VkSwapchainKHR,
    ) -> VkResult,
    pub destroy_swapchain_khr:
        unsafe extern "system" fn(VkDevice, VkSwapchainKHR, *const VkAllocationCallbacks),
    pub get_swapchain_images_khr: unsafe extern "system" fn(
        VkDevice,
        VkSwapchainKHR,
        *mut u32,
        *mut VkImage,
    ) -> VkResult,
    pub acquire_next_image_khr: unsafe extern "system" fn(
        VkDevice,
        VkSwapchainKHR,
        u64,
        VkSemaphore,
        VkFence,
        *mut u32,
    ) -> VkResult,
    pub queue_present_khr:
        unsafe extern "system" fn(VkQueue, *const VkPresentInfoKHR) -> VkResult,
}

impl DeviceFns {
    /// Resolves the device level table.
    ///
    /// Safety: the handle must be a live device created from the same instance,
    /// and the swapchain extension must have been enabled at creation.
    pub unsafe fn load(instance: &InstanceFns, device: VkDevice) -> Result<DeviceFns> {
        let get = instance.get_device_proc_addr;
        Ok(DeviceFns {
            destroy_device: load!(get, device, "vkDestroyDevice"),
            get_device_queue: load!(get, device, "vkGetDeviceQueue"),
            device_wait_idle: load!(get, device, "vkDeviceWaitIdle"),
            queue_submit: load!(get, device, "vkQueueSubmit"),

            create_command_pool: load!(get, device, "vkCreateCommandPool"),
            destroy_command_pool: load!(get, device, "vkDestroyCommandPool"),
            reset_command_pool: load!(get, device, "vkResetCommandPool"),
            allocate_command_buffers: load!(get, device, "vkAllocateCommandBuffers"),
            free_command_buffers: load!(get, device, "vkFreeCommandBuffers"),
            begin_command_buffer: load!(get, device, "vkBeginCommandBuffer"),
            end_command_buffer: load!(get, device, "vkEndCommandBuffer"),

            create_fence: load!(get, device, "vkCreateFence"),
            destroy_fence: load!(get, device, "vkDestroyFence"),
            reset_fences: load!(get, device, "vkResetFences"),
            wait_for_fences: load!(get, device, "vkWaitForFences"),
            create_semaphore: load!(get, device, "vkCreateSemaphore"),
            destroy_semaphore: load!(get, device, "vkDestroySemaphore"),
            create_query_pool: load!(get, device, "vkCreateQueryPool"),
            destroy_query_pool: load!(get, device, "vkDestroyQueryPool"),
            get_query_pool_results: load!(get, device, "vkGetQueryPoolResults"),
            cmd_reset_query_pool: load!(get, device, "vkCmdResetQueryPool"),
            cmd_write_timestamp: load!(get, device, "vkCmdWriteTimestamp"),

            create_buffer: load!(get, device, "vkCreateBuffer"),
            destroy_buffer: load!(get, device, "vkDestroyBuffer"),
            get_buffer_memory_requirements: load!(get, device, "vkGetBufferMemoryRequirements"),
            bind_buffer_memory: load!(get, device, "vkBindBufferMemory"),

            allocate_memory: load!(get, device, "vkAllocateMemory"),
            free_memory: load!(get, device, "vkFreeMemory"),
            map_memory: load!(get, device, "vkMapMemory"),
            unmap_memory: load!(get, device, "vkUnmapMemory"),
            flush_mapped_memory_ranges: load!(get, device, "vkFlushMappedMemoryRanges"),

            create_image: load!(get, device, "vkCreateImage"),
            destroy_image: load!(get, device, "vkDestroyImage"),
            get_image_memory_requirements: load!(get, device, "vkGetImageMemoryRequirements"),
            bind_image_memory: load!(get, device, "vkBindImageMemory"),
            create_image_view: load!(get, device, "vkCreateImageView"),
            destroy_image_view: load!(get, device, "vkDestroyImageView"),
            create_sampler: load!(get, device, "vkCreateSampler"),
            destroy_sampler: load!(get, device, "vkDestroySampler"),

            create_descriptor_set_layout: load!(get, device, "vkCreateDescriptorSetLayout"),
            destroy_descriptor_set_layout: load!(get, device, "vkDestroyDescriptorSetLayout"),
            create_descriptor_pool: load!(get, device, "vkCreateDescriptorPool"),
            destroy_descriptor_pool: load!(get, device, "vkDestroyDescriptorPool"),
            allocate_descriptor_sets: load!(get, device, "vkAllocateDescriptorSets"),
            update_descriptor_sets: load!(get, device, "vkUpdateDescriptorSets"),

            create_pipeline_layout: load!(get, device, "vkCreatePipelineLayout"),
            destroy_pipeline_layout: load!(get, device, "vkDestroyPipelineLayout"),
            create_shader_module: load!(get, device, "vkCreateShaderModule"),
            destroy_shader_module: load!(get, device, "vkDestroyShaderModule"),
            create_graphics_pipelines: load!(get, device, "vkCreateGraphicsPipelines"),
            destroy_pipeline: load!(get, device, "vkDestroyPipeline"),

            create_render_pass: load!(get, device, "vkCreateRenderPass"),
            destroy_render_pass: load!(get, device, "vkDestroyRenderPass"),
            create_framebuffer: load!(get, device, "vkCreateFramebuffer"),
            destroy_framebuffer: load!(get, device, "vkDestroyFramebuffer"),

            cmd_begin_render_pass: load!(get, device, "vkCmdBeginRenderPass"),
            cmd_end_render_pass: load!(get, device, "vkCmdEndRenderPass"),
            cmd_bind_pipeline: load!(get, device, "vkCmdBindPipeline"),
            cmd_set_viewport: load!(get, device, "vkCmdSetViewport"),
            cmd_set_scissor: load!(get, device, "vkCmdSetScissor"),
            cmd_push_constants: load!(get, device, "vkCmdPushConstants"),
            cmd_bind_vertex_buffers: load!(get, device, "vkCmdBindVertexBuffers"),
            cmd_bind_index_buffer: load!(get, device, "vkCmdBindIndexBuffer"),
            cmd_bind_descriptor_sets: load!(get, device, "vkCmdBindDescriptorSets"),
            cmd_draw_indexed: load!(get, device, "vkCmdDrawIndexed"),
            cmd_pipeline_barrier: load!(get, device, "vkCmdPipelineBarrier"),
            cmd_copy_buffer_to_image: load!(get, device, "vkCmdCopyBufferToImage"),
            cmd_clear_color_image: load!(get, device, "vkCmdClearColorImage"),

            create_swapchain_khr: load!(get, device, "vkCreateSwapchainKHR"),
            destroy_swapchain_khr: load!(get, device, "vkDestroySwapchainKHR"),
            get_swapchain_images_khr: load!(get, device, "vkGetSwapchainImagesKHR"),
            acquire_next_image_khr: load!(get, device, "vkAcquireNextImageKHR"),
            queue_present_khr: load!(get, device, "vkQueuePresentKHR"),
        })
    }
}