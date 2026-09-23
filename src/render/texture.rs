//! Sampled images and their descriptor sets.
//!
//! Every texture owns one descriptor set from a shared pool, so binding a
//! texture is a single command and the draw list only has to carry an integer.

/// 
use crate::core::{Error, Result};
use crate::render::device::Device;
use crate::render::memory::{Buffer, Location};
use crate::render::vk::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextureId(pub u32);

impl TextureId {
    pub const INVALID: TextureId = TextureId(u32::MAX);
}

pub struct Texture {
    pub image: VkImage,
    pub memory: VkDeviceMemory,
    pub view: VkImageView,
    pub extent: VkExtent2D,
    pub format: VkFormat,
    pub bytes_per_pixel: u32,
    pub descriptor: VkDescriptorSet,
    /// True while the descriptor names the nearest sampler.
    ///
    /// Held so a request that changes nothing costs nothing: rewriting the
    /// descriptor requires draining the device, which is not a per frame price.
    pub nearest: bool,
    /// Layout the image holds after the last operation.
    ///
    /// A partial upload must declare it as the old layout, otherwise the
    /// implementation is permitted to discard the contents outside the region
    /// being rewritten.
    pub layout: VkImageLayout,
}

pub struct TextureStore {
    textures: Vec<Texture>,
    pool: VkDescriptorPool,
    layout: VkDescriptorSetLayout,
    sampler_linear: VkSampler,
    sampler_nearest: VkSampler,
    capacity: u32,
}

impl TextureStore {
    pub fn new(
        device: &Device,
        layout: VkDescriptorSetLayout,
        capacity: u32,
    ) -> Result<TextureStore> {
        // One set beyond the stated capacity, for the auxiliary binding the
        // palette occupies. It is not a texture the application allocates, so
        // counting it against the operator limit would make that limit mean two
        // different things.
        let sets = capacity + 1;
        let size = VkDescriptorPoolSize {
            type_: VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
            descriptorCount: sets,
        };
        let pool_info = VkDescriptorPoolCreateInfo {
            maxSets: sets,
            poolSizeCount: 1,
            pPoolSizes: &size,
            ..Default::default()
        };
        let mut pool: VkDescriptorPool = VK_NULL_HANDLE;
        check("vkCreateDescriptorPool", unsafe {
            (device.fns.create_descriptor_pool)(
                device.handle,
                &pool_info,
                NO_ALLOCATOR,
                &mut pool,
            )
        })?;

        // Linear for images and the waterfall, nearest for the glyph atlas so a
        // hinted stem stays crisp at an integer scale.
        let sampler_linear = create_sampler(device, VK_FILTER_LINEAR)?;
        let sampler_nearest = create_sampler(device, VK_FILTER_NEAREST)?;

        Ok(TextureStore {
            textures: Vec::new(),
            pool,
            layout,
            sampler_linear,
            sampler_nearest,
            capacity,
        })
    }

    pub fn descriptor(&self, id: TextureId) -> Option<VkDescriptorSet> {
        self.textures.get(id.0 as usize).map(|t| t.descriptor)
    }

    pub fn extent(&self, id: TextureId) -> Option<VkExtent2D> {
        self.textures.get(id.0 as usize).map(|t| t.extent)
    }

    pub fn create_rgba8(
        &mut self,
        device: &Device,
        width: u32,
        height: u32,
        data: &[u8],
        nearest: bool,
    ) -> Result<TextureId> {
        self.create(device, width, height, VK_FORMAT_R8G8B8A8_UNORM, 4, data, nearest)
    }

    pub fn create_r8(
        &mut self,
        device: &Device,
        width: u32,
        height: u32,
        data: &[u8],
        nearest: bool,
    ) -> Result<TextureId> {
        self.create(device, width, height, VK_FORMAT_R8_UNORM, 1, data, nearest)
    }

    #[allow(clippy::too_many_arguments)]
    fn create(
        &mut self,
        device: &Device,
        width: u32,
        height: u32,
        format: VkFormat,
        bytes_per_pixel: u32,
        data: &[u8],
        nearest: bool,
    ) -> Result<TextureId> {
        if self.textures.len() as u32 >= self.capacity {
            return Err(Error::vulkan("texture descriptor pool exhausted"));
        }
        let expected = (width * height * bytes_per_pixel) as usize;
        if !data.is_empty() && data.len() < expected {
            return Err(Error::vulkan(format!(
                "texture data too small: {} bytes for {}x{}",
                data.len(),
                width,
                height
            )));
        }

        let image_info = VkImageCreateInfo {
            imageType: VK_IMAGE_TYPE_2D,
            format,
            extent: VkExtent3D { width, height, depth: 1 },
            mipLevels: 1,
            arrayLayers: 1,
            samples: VK_SAMPLE_COUNT_1_BIT,
            tiling: VK_IMAGE_TILING_OPTIMAL,
            usage: VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_SAMPLED_BIT,
            sharingMode: VK_SHARING_MODE_EXCLUSIVE,
            initialLayout: VK_IMAGE_LAYOUT_UNDEFINED,
            ..Default::default()
        };
        let mut image: VkImage = VK_NULL_HANDLE;
        check("vkCreateImage", unsafe {
            (device.fns.create_image)(device.handle, &image_info, NO_ALLOCATOR, &mut image)
        })?;

        let mut req = VkMemoryRequirements::default();
        unsafe {
            (device.fns.get_image_memory_requirements)(device.handle, image, &mut req);
        }
        let type_index = device
            .find_memory_type(req.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)
            .ok_or_else(|| Error::vulkan("no device local memory type for image"))?;

        let alloc = VkMemoryAllocateInfo {
            allocationSize: req.size,
            memoryTypeIndex: type_index,
            ..Default::default()
        };
        let mut memory: VkDeviceMemory = VK_NULL_HANDLE;
        check("vkAllocateMemory", unsafe {
            (device.fns.allocate_memory)(device.handle, &alloc, NO_ALLOCATOR, &mut memory)
        })?;
        check("vkBindImageMemory", unsafe {
            (device.fns.bind_image_memory)(device.handle, image, memory, 0)
        })?;

        let view_info = VkImageViewCreateInfo {
            image,
            viewType: VK_IMAGE_VIEW_TYPE_2D,
            format,
            subresourceRange: VkImageSubresourceRange {
                aspectMask: VK_IMAGE_ASPECT_COLOR_BIT,
                baseMipLevel: 0,
                levelCount: 1,
                baseArrayLayer: 0,
                layerCount: 1,
            },
            ..Default::default()
        };
        let mut view: VkImageView = VK_NULL_HANDLE;
        check("vkCreateImageView", unsafe {
            (device.fns.create_image_view)(device.handle, &view_info, NO_ALLOCATOR, &mut view)
        })?;

        let set_alloc = VkDescriptorSetAllocateInfo {
            descriptorPool: self.pool,
            descriptorSetCount: 1,
            pSetLayouts: &self.layout,
            ..Default::default()
        };
        let mut descriptor: VkDescriptorSet = VK_NULL_HANDLE;
        check("vkAllocateDescriptorSets", unsafe {
            (device.fns.allocate_descriptor_sets)(device.handle, &set_alloc, &mut descriptor)
        })?;

        let sampler = if nearest { self.sampler_nearest } else { self.sampler_linear };
        let image_binding = VkDescriptorImageInfo {
            sampler,
            imageView: view,
            imageLayout: VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        };
        let write = VkWriteDescriptorSet {
            dstSet: descriptor,
            dstBinding: 0,
            descriptorCount: 1,
            descriptorType: VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
            pImageInfo: &image_binding,
            ..Default::default()
        };
        unsafe {
            (device.fns.update_descriptor_sets)(
                device.handle,
                1,
                &write,
                0,
                std::ptr::null(),
            );
        }

        let id = TextureId(self.textures.len() as u32);
        self.textures.push(Texture {
            image,
            memory,
            view,
            extent: VkExtent2D { width, height },
            format,
            bytes_per_pixel,
            descriptor,
            nearest,
            layout: VK_IMAGE_LAYOUT_UNDEFINED,
        });

        // An empty upload still needs the transition, because sampling an image
        // in the undefined layout is invalid even when the shader discards the
        // result.
        if data.is_empty() {
            self.transition_only(device, id)?;
        } else {
            self.update_region(device, id, 0, 0, width, height, &data[..expected])?;
        }

        crate::log_debug!(
            "render",
            "texture {} created {}x{} {}",
            id.0,
            width,
            height,
            format_name(format)
        );
        Ok(id)
    }

    /// Uploads a sub rectangle through a staging buffer and blocks until done.
    ///
    /// The pre copy barrier declares the current layout, so the contents outside
    /// the region survive. It also carries the execution dependency against
    /// earlier submissions: a barrier applies to all work previously submitted
    /// to the same queue, which is what stops the copy from racing with an in
    /// flight frame that samples this image.
    #[allow(clippy::too_many_arguments)]
    pub fn update_region(
        &mut self,
        device: &Device,
        id: TextureId,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        data: &[u8],
    ) -> Result<()> {
        // Read out first so the borrow ends before the mutable update below.
        let (image, extent, bytes_per_pixel, old_layout) = {
            let tex = self
                .textures
                .get(id.0 as usize)
                .ok_or_else(|| Error::vulkan("update_region on an unknown texture"))?;
            (tex.image, tex.extent, tex.bytes_per_pixel, tex.layout)
        };

        let needed = (w as usize) * (h as usize) * (bytes_per_pixel as usize);
        if data.len() < needed {
            return Err(Error::vulkan("update_region data is shorter than the region"));
        }
        if x + w > extent.width || y + h > extent.height {
            return Err(Error::vulkan("update_region rectangle leaves the texture"));
        }
        if w == 0 || h == 0 {
            return Ok(());
        }

        let mut staging = Buffer::new(
            device,
            needed as VkDeviceSize,
            VK_BUFFER_USAGE_TRANSFER_SRC_BIT,
            Location::HostVisible,
        )?;
        staging.write(0, &data[..needed]);
        staging.flush(device)?;

        // A fresh image has no contents to preserve and nothing reads it yet, so
        // the source scope is empty; an established image was last read by the
        // fragment shader.
        let (src_stage, src_access) = if old_layout == VK_IMAGE_LAYOUT_UNDEFINED {
            (VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, 0)
        } else {
            (VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, VK_ACCESS_SHADER_READ_BIT)
        };

        let range = VkImageSubresourceRange {
            aspectMask: VK_IMAGE_ASPECT_COLOR_BIT,
            baseMipLevel: 0,
            levelCount: 1,
            baseArrayLayer: 0,
            layerCount: 1,
        };

        let result = device.one_time_submit(|cb| unsafe {
            let to_dst = VkImageMemoryBarrier {
                srcAccessMask: src_access,
                dstAccessMask: VK_ACCESS_TRANSFER_WRITE_BIT,
                oldLayout: old_layout,
                newLayout: VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                srcQueueFamilyIndex: VK_QUEUE_FAMILY_IGNORED,
                dstQueueFamilyIndex: VK_QUEUE_FAMILY_IGNORED,
                image,
                subresourceRange: range,
                ..Default::default()
            };
            (device.fns.cmd_pipeline_barrier)(
                cb,
                src_stage,
                VK_PIPELINE_STAGE_TRANSFER_BIT,
                0,
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                1,
                &to_dst,
            );

            // A zero row length and image height mean the payload is tightly
            // packed for the region rather than for the full texture width.
            let region = VkBufferImageCopy {
                bufferOffset: 0,
                bufferRowLength: 0,
                bufferImageHeight: 0,
                imageSubresource: VkImageSubresourceLayers {
                    aspectMask: VK_IMAGE_ASPECT_COLOR_BIT,
                    mipLevel: 0,
                    baseArrayLayer: 0,
                    layerCount: 1,
                },
                imageOffset: VkOffset3D { x: x as i32, y: y as i32, z: 0 },
                imageExtent: VkExtent3D { width: w, height: h, depth: 1 },
            };
            (device.fns.cmd_copy_buffer_to_image)(
                cb,
                staging.handle,
                image,
                VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                1,
                &region,
            );

            let to_read = VkImageMemoryBarrier {
                srcAccessMask: VK_ACCESS_TRANSFER_WRITE_BIT,
                dstAccessMask: VK_ACCESS_SHADER_READ_BIT,
                oldLayout: VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                newLayout: VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                srcQueueFamilyIndex: VK_QUEUE_FAMILY_IGNORED,
                dstQueueFamilyIndex: VK_QUEUE_FAMILY_IGNORED,
                image,
                subresourceRange: range,
                ..Default::default()
            };
            (device.fns.cmd_pipeline_barrier)(
                cb,
                VK_PIPELINE_STAGE_TRANSFER_BIT,
                VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT,
                0,
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                1,
                &to_read,
            );
        });

        staging.destroy(device);

        if result.is_ok() {
            if let Some(tex) = self.textures.get_mut(id.0 as usize) {
                tex.layout = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
            }
        }
        result
    }

    /// Moves a freshly created image into the sampled layout without data.
    fn transition_only(&mut self, device: &Device, id: TextureId) -> Result<()> {
        let image = match self.textures.get(id.0 as usize) {
            Some(t) => t.image,
            None => return Err(Error::vulkan("transition on an unknown texture")),
        };

        let result = device.one_time_submit(|cb| unsafe {
            let barrier = VkImageMemoryBarrier {
                srcAccessMask: 0,
                dstAccessMask: VK_ACCESS_SHADER_READ_BIT,
                oldLayout: VK_IMAGE_LAYOUT_UNDEFINED,
                newLayout: VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                srcQueueFamilyIndex: VK_QUEUE_FAMILY_IGNORED,
                dstQueueFamilyIndex: VK_QUEUE_FAMILY_IGNORED,
                image,
                subresourceRange: VkImageSubresourceRange {
                    aspectMask: VK_IMAGE_ASPECT_COLOR_BIT,
                    baseMipLevel: 0,
                    levelCount: 1,
                    baseArrayLayer: 0,
                    layerCount: 1,
                },
                ..Default::default()
            };
            (device.fns.cmd_pipeline_barrier)(
                cb,
                VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,
                VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT,
                0,
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                1,
                &barrier,
            );
        });

        if result.is_ok() {
            if let Some(tex) = self.textures.get_mut(id.0 as usize) {
                tex.layout = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
            }
        }
        result
    }

    /// Allocates the auxiliary set and points it at an existing texture.
    ///
    /// A set of its own rather than a second binding in every per texture set:
    /// the palette is the same for every draw command, so a per texture binding
    /// would write the same descriptor into the set of every glyph atlas and
    /// would have to be rewritten whenever the palette moved.
    pub fn create_aux_set(
        &mut self,
        device: &Device,
        layout: VkDescriptorSetLayout,
        id: TextureId,
        nearest: bool,
    ) -> Result<VkDescriptorSet> {
        let view = self
            .textures
            .get(id.0 as usize)
            .map(|t| t.view)
            .ok_or_else(|| Error::vulkan("create_aux_set on an unknown texture"))?;

        let alloc = VkDescriptorSetAllocateInfo {
            descriptorPool: self.pool,
            descriptorSetCount: 1,
            pSetLayouts: &layout,
            ..Default::default()
        };
        let mut set: VkDescriptorSet = VK_NULL_HANDLE;
        check("vkAllocateDescriptorSets", unsafe {
            (device.fns.allocate_descriptor_sets)(device.handle, &alloc, &mut set)
        })?;

        let binding = VkDescriptorImageInfo {
            sampler: if nearest { self.sampler_nearest } else { self.sampler_linear },
            imageView: view,
            imageLayout: VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        };
        let write = VkWriteDescriptorSet {
            dstSet: set,
            dstBinding: 0,
            descriptorCount: 1,
            descriptorType: VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
            pImageInfo: &binding,
            ..Default::default()
        };
        unsafe {
            (device.fns.update_descriptor_sets)(device.handle, 1, &write, 0, std::ptr::null());
        }
        Ok(set)
    }

    /// True when a filter request would rewrite the descriptor.
    pub fn would_change_filter(&self, id: TextureId, nearest: bool) -> bool {
        self.textures
            .get(id.0 as usize)
            .map(|t| t.nearest != nearest)
            .unwrap_or(false)
    }

    /// Points an existing descriptor at the other sampler.
    ///
    /// Returns false when the descriptor already names it, so the caller can
    /// skip the device drain the rewrite needs.
    ///
    /// Safety of the rewrite is the caller's: a descriptor set referenced by a
    /// submission still in flight must not be updated, so the device has to be
    /// idle before this is called.
    pub fn set_filter(&mut self, device: &Device, id: TextureId, nearest: bool) -> bool {
        let (view, descriptor) = match self.textures.get(id.0 as usize) {
            Some(t) if t.nearest != nearest => (t.view, t.descriptor),
            _ => return false,
        };

        let binding = VkDescriptorImageInfo {
            sampler: if nearest { self.sampler_nearest } else { self.sampler_linear },
            imageView: view,
            imageLayout: VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        };
        let write = VkWriteDescriptorSet {
            dstSet: descriptor,
            dstBinding: 0,
            descriptorCount: 1,
            descriptorType: VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
            pImageInfo: &binding,
            ..Default::default()
        };
        unsafe {
            (device.fns.update_descriptor_sets)(device.handle, 1, &write, 0, std::ptr::null());
        }

        if let Some(t) = self.textures.get_mut(id.0 as usize) {
            t.nearest = nearest;
        }
        true
    }

    /// Everything the deferred upload path needs to record a copy.
    pub fn info(&self, id: TextureId) -> Option<(VkImage, VkExtent2D, u32, VkImageLayout)> {
        self.textures
            .get(id.0 as usize)
            .map(|t| (t.image, t.extent, t.bytes_per_pixel, t.layout))
    }

    /// Records that a region reached the sampled layout.
    ///
    /// Called after the frame command buffer has been recorded, because the
    /// deferred path performs the transition itself and the store would
    /// otherwise keep declaring the old layout as the source scope of the next
    /// upload.
    pub fn note_uploaded(&mut self, id: TextureId) {
        if let Some(tex) = self.textures.get_mut(id.0 as usize) {
            tex.layout = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
        }
    }

    pub fn destroy(&mut self, device: &Device) {
        unsafe {
            for t in self.textures.drain(..) {
                (device.fns.destroy_image_view)(device.handle, t.view, NO_ALLOCATOR);
                (device.fns.destroy_image)(device.handle, t.image, NO_ALLOCATOR);
                (device.fns.free_memory)(device.handle, t.memory, NO_ALLOCATOR);
            }
            (device.fns.destroy_sampler)(device.handle, self.sampler_linear, NO_ALLOCATOR);
            (device.fns.destroy_sampler)(device.handle, self.sampler_nearest, NO_ALLOCATOR);
            // Descriptor sets are released together with the pool.
            (device.fns.destroy_descriptor_pool)(device.handle, self.pool, NO_ALLOCATOR);
        }
        self.sampler_linear = VK_NULL_HANDLE;
        self.sampler_nearest = VK_NULL_HANDLE;
        self.pool = VK_NULL_HANDLE;
    }
}

fn create_sampler(device: &Device, filter: VkFilter) -> Result<VkSampler> {
    // Clamped on every axis: the atlas packs unrelated glyphs next to each
    // other, and repeating would sample a neighbour into the edge of a glyph.
    let info = VkSamplerCreateInfo {
        magFilter: filter,
        minFilter: filter,
        mipmapMode: VK_SAMPLER_MIPMAP_MODE_NEAREST,
        addressModeU: VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
        addressModeV: VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
        addressModeW: VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
        minLod: 0.0,
        maxLod: 0.0,
        borderColor: VK_BORDER_COLOR_FLOAT_TRANSPARENT_BLACK,
        ..Default::default()
    };
    let mut sampler: VkSampler = VK_NULL_HANDLE;
    check("vkCreateSampler", unsafe {
        (device.fns.create_sampler)(device.handle, &info, NO_ALLOCATOR, &mut sampler)
    })?;
    Ok(sampler)
}