//! Per frame resources: command pool, command buffer, fence, acquire semaphore,
//! the geometry buffers written every frame and the staging buffer that carries
//! texture updates.
//!
//! Staging lives per frame rather than per upload because the copy is recorded
//! into the same command buffer as the draw calls. The frame fence therefore
//! guarantees the buffer is free before it is rewritten, and no separate
//! submission or fence wait is needed.

use crate::core::Result;
use crate::render::batch::DrawList;
use crate::render::device::Device;
use crate::render::memory::{as_bytes, DynamicBuffer};
use crate::render::vk::*;

pub struct Frame {
    pub command_pool: VkCommandPool,
    pub command_buffer: VkCommandBuffer,
    pub in_flight: VkFence,
    pub image_available: VkSemaphore,
    pub vertex: DynamicBuffer,
    pub index: DynamicBuffer,
    pub staging: DynamicBuffer,
}

impl Frame {
    pub fn new(
        device: &Device,
        vertex_bytes: usize,
        index_bytes: usize,
        staging_bytes: usize,
        slot: u32,
    ) -> Result<Frame> {
        // A pool per frame lets the whole pool be reset at once rather than
        // resetting individual buffers, which is the cheaper path and does not
        // require the reset flag on the pool.
        let pool_info = VkCommandPoolCreateInfo {
            flags: VK_COMMAND_POOL_CREATE_TRANSIENT_BIT,
            queueFamilyIndex: device.graphics_family,
            ..Default::default()
        };
        let mut command_pool: VkCommandPool = VK_NULL_HANDLE;
        check("vkCreateCommandPool", unsafe {
            (device.fns.create_command_pool)(
                device.handle,
                &pool_info,
                NO_ALLOCATOR,
                &mut command_pool,
            )
        })?;

        let alloc = VkCommandBufferAllocateInfo {
            commandPool: command_pool,
            level: VK_COMMAND_BUFFER_LEVEL_PRIMARY,
            commandBufferCount: 1,
            ..Default::default()
        };
        let mut command_buffer: VkCommandBuffer = std::ptr::null_mut();
        check("vkAllocateCommandBuffers", unsafe {
            (device.fns.allocate_command_buffers)(device.handle, &alloc, &mut command_buffer)
        })?;

        // Created signalled so the first wait returns immediately.
        let fence_info = VkFenceCreateInfo {
            flags: VK_FENCE_CREATE_SIGNALED_BIT,
            ..Default::default()
        };
        let mut in_flight: VkFence = VK_NULL_HANDLE;
        check("vkCreateFence", unsafe {
            (device.fns.create_fence)(device.handle, &fence_info, NO_ALLOCATOR, &mut in_flight)
        })?;

        let sem_info = VkSemaphoreCreateInfo::default();
        let mut image_available: VkSemaphore = VK_NULL_HANDLE;
        check("vkCreateSemaphore", unsafe {
            (device.fns.create_semaphore)(
                device.handle,
                &sem_info,
                NO_ALLOCATOR,
                &mut image_available,
            )
        })?;

        let vertex = DynamicBuffer::new(device, vertex_bytes, VK_BUFFER_USAGE_VERTEX_BUFFER_BIT)?;
        let index = DynamicBuffer::new(device, index_bytes, VK_BUFFER_USAGE_INDEX_BUFFER_BIT)?;
        let staging = DynamicBuffer::new(device, staging_bytes, VK_BUFFER_USAGE_TRANSFER_SRC_BIT)?;

        crate::log_debug!(
            "render",
            "frame {} allocated, vertex {} KB index {} KB staging {} KB",
            slot,
            vertex_bytes / 1024,
            index_bytes / 1024,
            staging_bytes / 1024
        );

        Ok(Frame {
            command_pool,
            command_buffer,
            in_flight,
            image_available,
            vertex,
            index,
            staging,
        })
    }

    /// Copies the draw list geometry into the mapped buffers.
    ///
    /// Called after the frame fence has been waited on, so the previous contents
    /// are no longer referenced by the device.
    pub fn upload(&mut self, device: &Device, list: &DrawList) -> Result<()> {
        let vertex_bytes = as_bytes(&list.vertices);
        let index_bytes = as_bytes(&list.indices);

        self.vertex.reserve(device, vertex_bytes.len())?;
        self.index.reserve(device, index_bytes.len())?;

        self.vertex.write(vertex_bytes);
        self.index.write(index_bytes);

        self.vertex.buffer.flush(device)?;
        self.index.buffer.flush(device)?;
        Ok(())
    }

    /// Copies the texture update payload for this frame.
    pub fn upload_staging(&mut self, device: &Device, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.staging.reserve(device, bytes.len())?;
        self.staging.write(bytes);
        self.staging.buffer.flush(device)
    }

    pub fn destroy(mut self, device: &Device) {
        unsafe {
            (device.fns.destroy_semaphore)(device.handle, self.image_available, NO_ALLOCATOR);
            (device.fns.destroy_fence)(device.handle, self.in_flight, NO_ALLOCATOR);
            (device.fns.destroy_command_pool)(device.handle, self.command_pool, NO_ALLOCATOR);
        }
        self.vertex.destroy(device);
        self.index.destroy(device);
        self.staging.destroy(device);
    }
}