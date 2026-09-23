//! Buffer allocation helpers.
//!
//! One allocation per buffer. That is acceptable because the renderer creates a
//! handful of long lived buffers and never allocates per object; a suballocator
//! only pays for itself once the second pattern appears.

use std::ffi::c_void;

use crate::core::{Error, Result};
use crate::render::device::Device;
use crate::render::vk::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// Mapped permanently, written by the processor each frame.
    HostVisible,
    /// Fast device memory, filled through a staging copy.
    DeviceLocal,
}

pub struct Buffer {
    pub handle: VkBuffer,
    pub memory: VkDeviceMemory,
    pub size: VkDeviceSize,
    /// Null for a device local allocation.
    pub mapped: *mut u8,
    coherent: bool,
}

impl Buffer {
    pub fn new(
        device: &Device,
        size: VkDeviceSize,
        usage: VkBufferUsageFlags,
        location: Location,
    ) -> Result<Buffer> {
        let size = size.max(256);
        let info = VkBufferCreateInfo {
            size,
            usage,
            sharingMode: VK_SHARING_MODE_EXCLUSIVE,
            ..Default::default()
        };

        let mut handle: VkBuffer = VK_NULL_HANDLE;
        check("vkCreateBuffer", unsafe {
            (device.fns.create_buffer)(device.handle, &info, NO_ALLOCATOR, &mut handle)
        })?;

        let mut req = VkMemoryRequirements::default();
        unsafe {
            (device.fns.get_buffer_memory_requirements)(device.handle, handle, &mut req);
        }

        // Coherent host memory is preferred so no flush is needed. When the
        // implementation offers none, the writes are flushed explicitly.
        let (wanted, fallback) = match location {
            Location::HostVisible => (
                VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT,
                VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT,
            ),
            Location::DeviceLocal => (
                VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT,
                VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT,
            ),
        };
        let (type_index, coherent) = match device.find_memory_type(req.memoryTypeBits, wanted) {
            Some(i) => (i, true),
            None => {
                let i = device
                    .find_memory_type(req.memoryTypeBits, fallback)
                    .ok_or_else(|| Error::vulkan("no compatible memory type for buffer"))?;
                (i, false)
            }
        };

        let alloc = VkMemoryAllocateInfo {
            allocationSize: req.size,
            memoryTypeIndex: type_index,
            ..Default::default()
        };
        let mut memory: VkDeviceMemory = VK_NULL_HANDLE;
        check("vkAllocateMemory", unsafe {
            (device.fns.allocate_memory)(device.handle, &alloc, NO_ALLOCATOR, &mut memory)
        })?;
        check("vkBindBufferMemory", unsafe {
            (device.fns.bind_buffer_memory)(device.handle, handle, memory, 0)
        })?;

        let mapped = if location == Location::HostVisible {
            let mut address: *mut c_void = std::ptr::null_mut();
            check("vkMapMemory", unsafe {
                (device.fns.map_memory)(
                    device.handle,
                    memory,
                    0,
                    VK_WHOLE_SIZE,
                    0,
                    &mut address,
                )
            })?;
            address as *mut u8
        } else {
            std::ptr::null_mut()
        };

        Ok(Buffer { handle, memory, size, mapped, coherent })
    }

    /// Copies into the mapped range.
    ///
    /// The caller guarantees the buffer is not in use by the device, normally by
    /// waiting on the frame fence first.
    pub fn write(&self, offset: VkDeviceSize, data: &[u8]) {
        debug_assert!(!self.mapped.is_null(), "write on a device local buffer");
        debug_assert!(offset + data.len() as VkDeviceSize <= self.size);
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                self.mapped.add(offset as usize),
                data.len(),
            );
        }
    }

    /// Makes host writes visible to the device.
    ///
    /// The whole allocation is flushed from offset nought, which the
    /// specification permits at any non coherent atom size, so the device limit
    /// never has to be consulted. A partial range would have to be rounded to
    /// that atom, and the saving would be nothing: these buffers are rewritten
    /// in full every frame.
    pub fn flush(&self, device: &Device) -> Result<()> {
        if self.coherent || self.mapped.is_null() {
            return Ok(());
        }
        let range = VkMappedMemoryRange {
            memory: self.memory,
            offset: 0,
            size: VK_WHOLE_SIZE,
            ..Default::default()
        };
        check("vkFlushMappedMemoryRanges", unsafe {
            (device.fns.flush_mapped_memory_ranges)(device.handle, 1, &range)
        })
    }

    pub fn destroy(&mut self, device: &Device) {
        unsafe {
            if !self.mapped.is_null() {
                (device.fns.unmap_memory)(device.handle, self.memory);
                self.mapped = std::ptr::null_mut();
            }
            if self.handle != VK_NULL_HANDLE {
                (device.fns.destroy_buffer)(device.handle, self.handle, NO_ALLOCATOR);
            }
            if self.memory != VK_NULL_HANDLE {
                (device.fns.free_memory)(device.handle, self.memory, NO_ALLOCATOR);
            }
        }
        self.handle = VK_NULL_HANDLE;
        self.memory = VK_NULL_HANDLE;
    }
}

/// Host visible buffer that grows when a frame needs more room.
///
/// Growth is geometric so a busy waterfall does not reallocate every frame.
pub struct DynamicBuffer {
    pub buffer: Buffer,
    usage: VkBufferUsageFlags,
}

impl DynamicBuffer {
    pub fn new(
        device: &Device,
        size: usize,
        usage: VkBufferUsageFlags,
    ) -> Result<DynamicBuffer> {
        let buffer = Buffer::new(device, size as VkDeviceSize, usage, Location::HostVisible)?;
        Ok(DynamicBuffer { buffer, usage })
    }

    /// Ensures capacity.
    ///
    /// Safe only when the previous contents are no longer referenced by the
    /// device, because the old allocation is released immediately.
    pub fn reserve(&mut self, device: &Device, bytes: usize) -> Result<()> {
        if bytes as VkDeviceSize <= self.buffer.size {
            return Ok(());
        }
        let mut new_size = self.buffer.size.max(256);
        while new_size < bytes as VkDeviceSize {
            new_size *= 2;
        }
        crate::log_debug!(
            "render",
            "dynamic buffer grows {} to {} bytes",
            self.buffer.size,
            new_size
        );
        let mut old = std::mem::replace(
            &mut self.buffer,
            Buffer::new(device, new_size, self.usage, Location::HostVisible)?,
        );
        old.destroy(device);
        Ok(())
    }

    pub fn write(&self, data: &[u8]) {
        if !data.is_empty() {
            self.buffer.write(0, data);
        }
    }

    pub fn destroy(&mut self, device: &Device) {
        self.buffer.destroy(device);
    }
}

/// Reinterprets a typed slice as bytes for the upload path.
pub fn as_bytes<T: Copy>(slice: &[T]) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(slice.as_ptr() as *const u8, std::mem::size_of_val(slice))
    }
}