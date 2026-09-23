//! Physical device selection, logical device, queues and memory helpers.
//!
//! Only the two properties the selection consults are retained from the device
//! properties structure: the application requests no feature and depends on no
//! limit, so keeping the rest would be keeping a hundred and thirty fields
//! nothing reads.

use std::ffi::c_char;

use crate::config::settings::RenderSettings;
use crate::core::{Error, Result};
use crate::render::instance::Instance;
use crate::render::surface::Surface;
use crate::render::vk::*;

pub struct Device {
    pub physical: VkPhysicalDevice,
    pub handle: VkDevice,
    pub fns: DeviceFns,
    pub graphics_family: u32,
    pub present_family: u32,
    pub graphics_queue: VkQueue,
    pub present_queue: VkQueue,
    pub memory_props: VkPhysicalDeviceMemoryProperties,
    pub api_version: u32,
    pub device_type: VkPhysicalDeviceType,
    /// Nanoseconds one timestamp tick represents, nought when unsupported.
    pub timestamp_period_ns: f32,
    /// Meaningful bits in a timestamp result. Nought means the graphics queue
    /// cannot write one at all, which is permitted and does occur on a queue
    /// dedicated to transfer.
    pub timestamp_valid_bits: u32,
    pub name: String,
    /// Short lived command buffers for uploads and layout transitions.
    transient_pool: VkCommandPool,
    /// Reused fence for one time submits, so an upload does not create one.
    upload_fence: VkFence,
}

struct Candidate {
    physical: VkPhysicalDevice,
    graphics: u32,
    present: u32,
    score: i64,
    name: String,
    api_version: u32,
    device_type: VkPhysicalDeviceType,
    timestamp_period_ns: f32,
    timestamp_valid_bits: u32,
}

impl Device {
    pub fn new(instance: &Instance, surface: &Surface, cfg: &RenderSettings) -> Result<Device> {
        let physicals = enumerate(|count, data| unsafe {
            (instance.fns.enumerate_physical_devices)(instance.handle, count, data)
        })
        .map_err(|r| vk_err("vkEnumeratePhysicalDevices", r))?;

        if physicals.is_empty() {
            return Err(Error::vulkan("no Vulkan capable device found"));
        }

        let mut candidates: Vec<Candidate> = Vec::with_capacity(physicals.len());
        for (index, &physical) in physicals.iter().enumerate() {
            let mut props = VkPhysicalDeviceProperties::default();
            unsafe {
                (instance.fns.get_physical_device_properties)(physical, &mut props);
            }
            let name = name_from_array(&props.deviceName);

            // Presenting is mandatory. A compute only device is of no use to a
            // renderer whose only output is a window.
            let extensions = match enumerate(|count, data| unsafe {
                (instance.fns.enumerate_device_extension_properties)(
                    physical,
                    std::ptr::null(),
                    count,
                    data,
                )
            }) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let has_swapchain = extensions.iter().any(|e: &VkExtensionProperties| {
                name_matches(&e.extensionName, VK_KHR_SWAPCHAIN_EXTENSION_NAME)
            });
            if !has_swapchain {
                crate::log_debug!("render", "device {} skipped: no swapchain extension", name);
                continue;
            }

            let families = queue_families(instance, physical);

            // A family that does both removes the ownership transfer and the
            // concurrent sharing mode, so it is preferred over a pair even when
            // the pair is otherwise equivalent.
            let mut graphics = None;
            let mut present = None;
            let mut combined = None;
            for (i, family) in families.iter().enumerate() {
                let i = i as u32;
                let can_draw = family.queueFlags & VK_QUEUE_GRAPHICS_BIT != 0;
                let can_present = surface.supports(instance, physical, i);
                if can_draw && can_present && combined.is_none() {
                    combined = Some(i);
                }
                if can_draw && graphics.is_none() {
                    graphics = Some(i);
                }
                if can_present && present.is_none() {
                    present = Some(i);
                }
            }
            let (graphics, present) = match combined {
                Some(c) => (c, c),
                None => match (graphics, present) {
                    (Some(g), Some(p)) => (g, p),
                    _ => {
                        crate::log_debug!("render", "device {} skipped: no suitable queues", name);
                        continue;
                    }
                },
            };

            let mut score: i64 = match props.deviceType {
                VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU => 1000,
                VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU => 500,
                VK_PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU => 100,
                VK_PHYSICAL_DEVICE_TYPE_CPU => 10,
                _ => 1,
            };
            if graphics == present {
                score += 50;
            }
            // An operator choice outweighs every heuristic, and an index
            // outweighs a name because it identifies one device rather than a
            // family of them.
            if cfg.device_index >= 0 && cfg.device_index as usize == index {
                score += 100_000;
            }
            if !cfg.device_name.is_empty()
                && name.to_lowercase().contains(&cfg.device_name.to_lowercase())
            {
                score += 50_000;
            }

            if cfg.log_device_info {
                crate::log_info!(
                    "render",
                    "device {}: {} type {} api {}.{}.{} score {}",
                    index,
                    name,
                    physical_device_type_name(props.deviceType),
                    api_version_major(props.apiVersion),
                    api_version_minor(props.apiVersion),
                    api_version_patch(props.apiVersion),
                    score
                );
            }

            // Read from the family that will carry the work rather than from the
            // first family that reports any: a queue may support graphics and
            // still report no timestamp bits.
            let timestamp_valid_bits = families
                .get(graphics as usize)
                .map(|f| f.timestampValidBits)
                .unwrap_or(0);

            candidates.push(Candidate {
                physical,
                graphics,
                present,
                score,
                name,
                api_version: props.apiVersion,
                device_type: props.deviceType,
                timestamp_period_ns: props.timestampPeriod,
                timestamp_valid_bits,
            });
        }

        let best = candidates
            .into_iter()
            .max_by_key(|c| c.score)
            .ok_or_else(|| Error::vulkan("no device supports presenting to the window"))?;

        let priorities = [1.0f32];
        let mut queue_infos = vec![VkDeviceQueueCreateInfo {
            queueFamilyIndex: best.graphics,
            queueCount: 1,
            pQueuePriorities: priorities.as_ptr(),
            ..Default::default()
        }];
        if best.present != best.graphics {
            queue_infos.push(VkDeviceQueueCreateInfo {
                queueFamilyIndex: best.present,
                queueCount: 1,
                pQueuePriorities: priorities.as_ptr(),
                ..Default::default()
            });
        }

        let device_extensions = [VK_KHR_SWAPCHAIN_EXTENSION_NAME.as_ptr() as *const c_char];
        let device_info = VkDeviceCreateInfo {
            queueCreateInfoCount: queue_infos.len() as u32,
            pQueueCreateInfos: queue_infos.as_ptr(),
            enabledExtensionCount: device_extensions.len() as u32,
            ppEnabledExtensionNames: device_extensions.as_ptr(),
            // Null rather than a zeroed feature structure. The two are
            // equivalent to the driver, and the structure holds fifty five
            // booleans whose only correct value here is false.
            pEnabledFeatures: std::ptr::null(),
            ..Default::default()
        };

        let mut handle: VkDevice = std::ptr::null_mut();
        check("vkCreateDevice", unsafe {
            (instance.fns.create_device)(best.physical, &device_info, NO_ALLOCATOR, &mut handle)
        })?;
        if handle.is_null() {
            return Err(Error::vulkan("vkCreateDevice returned a null handle"));
        }

        let fns = unsafe { DeviceFns::load(&instance.fns, handle)? };

        let mut graphics_queue: VkQueue = std::ptr::null_mut();
        let mut present_queue: VkQueue = std::ptr::null_mut();
        unsafe {
            (fns.get_device_queue)(handle, best.graphics, 0, &mut graphics_queue);
            (fns.get_device_queue)(handle, best.present, 0, &mut present_queue);
        }

        let mut memory_props = VkPhysicalDeviceMemoryProperties::default();
        unsafe {
            (instance.fns.get_physical_device_memory_properties)(best.physical, &mut memory_props);
        }

        // Transient tells the driver the buffers are short lived, which lets it
        // use a cheaper allocation strategy inside the pool.
        let pool_info = VkCommandPoolCreateInfo {
            flags: VK_COMMAND_POOL_CREATE_TRANSIENT_BIT,
            queueFamilyIndex: best.graphics,
            ..Default::default()
        };
        let mut transient_pool: VkCommandPool = VK_NULL_HANDLE;
        check("vkCreateCommandPool", unsafe {
            (fns.create_command_pool)(handle, &pool_info, NO_ALLOCATOR, &mut transient_pool)
        })?;

        let fence_info = VkFenceCreateInfo::default();
        let mut upload_fence: VkFence = VK_NULL_HANDLE;
        check("vkCreateFence", unsafe {
            (fns.create_fence)(handle, &fence_info, NO_ALLOCATOR, &mut upload_fence)
        })?;

        crate::log_info!(
            "render",
            "using {} (graphics family {}, present family {})",
            best.name,
            best.graphics,
            best.present
        );

        Ok(Device {
            physical: best.physical,
            handle,
            fns,
            graphics_family: best.graphics,
            present_family: best.present,
            graphics_queue,
            present_queue,
            memory_props,
            api_version: best.api_version,
            device_type: best.device_type,
            timestamp_period_ns: best.timestamp_period_ns,
            timestamp_valid_bits: best.timestamp_valid_bits,
            name: best.name,
            transient_pool,
            upload_fence,
        })
    }

    /// Picks a memory type that satisfies both the resource bits and the
    /// requested properties.
    pub fn find_memory_type(
        &self,
        type_bits: u32,
        flags: VkMemoryPropertyFlags,
    ) -> Option<u32> {
        for i in 0..self.memory_props.memoryTypeCount {
            let supported = (type_bits & (1 << i)) != 0;
            let props = self.memory_props.memoryTypes[i as usize].propertyFlags;
            if supported && props & flags == flags {
                return Some(i);
            }
        }
        None
    }

    /// Records and submits a short command buffer, then blocks until it is done.
    ///
    /// Used at load time and for immediate texture uploads. The per frame path
    /// records its copies into the frame command buffer instead, so this never
    /// appears on the presentation path.
    pub fn one_time_submit<F: FnOnce(VkCommandBuffer)>(&self, record: F) -> Result<()> {
        let alloc = VkCommandBufferAllocateInfo {
            commandPool: self.transient_pool,
            level: VK_COMMAND_BUFFER_LEVEL_PRIMARY,
            commandBufferCount: 1,
            ..Default::default()
        };

        let mut command_buffer: VkCommandBuffer = std::ptr::null_mut();
        check("vkAllocateCommandBuffers", unsafe {
            (self.fns.allocate_command_buffers)(self.handle, &alloc, &mut command_buffer)
        })?;

        let result = self.record_and_wait(command_buffer, record);

        unsafe {
            (self.fns.free_command_buffers)(
                self.handle,
                self.transient_pool,
                1,
                &command_buffer,
            );
        }
        result
    }

    /// Body of the one time submit, split out so the buffer is freed on every
    /// path including the failing ones.
    fn record_and_wait<F: FnOnce(VkCommandBuffer)>(
        &self,
        command_buffer: VkCommandBuffer,
        record: F,
    ) -> Result<()> {
        let begin = VkCommandBufferBeginInfo {
            flags: VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
            ..Default::default()
        };
        check("vkBeginCommandBuffer", unsafe {
            (self.fns.begin_command_buffer)(command_buffer, &begin)
        })?;

        record(command_buffer);

        check("vkEndCommandBuffer", unsafe {
            (self.fns.end_command_buffer)(command_buffer)
        })?;

        let submit = VkSubmitInfo {
            commandBufferCount: 1,
            pCommandBuffers: &command_buffer,
            ..Default::default()
        };

        check("vkResetFences", unsafe {
            (self.fns.reset_fences)(self.handle, 1, &self.upload_fence)
        })?;
        check("vkQueueSubmit", unsafe {
            (self.fns.queue_submit)(self.graphics_queue, 1, &submit, self.upload_fence)
        })?;
        check("vkWaitForFences", unsafe {
            (self.fns.wait_for_fences)(self.handle, 1, &self.upload_fence, VK_TRUE, u64::MAX)
        })?;
        Ok(())
    }

    /// True when the graphics queue can write a timestamp that means something.
    ///
    /// Both conditions are needed. A queue with no valid bits cannot write one at
    /// all; a period of nought would make every interval read as instantaneous,
    /// which is worse than reporting nothing.
    pub fn supports_timestamps(&self) -> bool {
        self.timestamp_valid_bits > 0 && self.timestamp_period_ns > 0.0
    }

    /// Masks a timestamp to the bits the implementation guarantees.
    ///
    /// The specification leaves the bits above the reported width undefined, so a
    /// difference taken without masking would carry whatever the driver left
    /// there. Modular subtraction inside the masked width then gives the correct
    /// interval even across a wrap, provided the interval is shorter than the
    /// counter period, which at nanosecond resolution and thirty six bits is over
    /// a minute.
    pub fn mask_timestamp(&self, value: u64) -> u64 {
        if self.timestamp_valid_bits >= 64 {
            value
        } else {
            value & ((1u64 << self.timestamp_valid_bits) - 1)
        }
    }

    pub fn wait_idle(&self) -> Result<()> {
        check("vkDeviceWaitIdle", unsafe {
            (self.fns.device_wait_idle)(self.handle)
        })
    }

    pub fn destroy(&mut self) {
        unsafe {
            (self.fns.destroy_fence)(self.handle, self.upload_fence, NO_ALLOCATOR);
            (self.fns.destroy_command_pool)(self.handle, self.transient_pool, NO_ALLOCATOR);
            (self.fns.destroy_device)(self.handle, NO_ALLOCATOR);
        }
        self.upload_fence = VK_NULL_HANDLE;
        self.transient_pool = VK_NULL_HANDLE;
        self.handle = std::ptr::null_mut();
    }
}

/// Reads the queue family list.
///
/// Written out rather than routed through the generic enumeration helper,
/// because this query returns no result code and the helper is built around one.
fn queue_families(instance: &Instance, physical: VkPhysicalDevice) -> Vec<VkQueueFamilyProperties> {
    let mut count: u32 = 0;
    unsafe {
        (instance.fns.get_physical_device_queue_family_properties)(
            physical,
            &mut count,
            std::ptr::null_mut(),
        );
    }
    if count == 0 {
        return Vec::new();
    }
    let mut families = vec![VkQueueFamilyProperties::default(); count as usize];
    unsafe {
        (instance.fns.get_physical_device_queue_family_properties)(
            physical,
            &mut count,
            families.as_mut_ptr(),
        );
    }
    families.truncate(count as usize);
    families
}