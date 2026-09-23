//! Vulkan renderer facade.
//!
//! Ownership chain: `Instance` owns the loader, the instance handle and the
//! instance level dispatch table; `Device` owns the logical device, the queues
//! and the device level table; `Surface` owns the surface; `Swapchain` owns the
//! images, views, render pass and framebuffers. Frames own the per frame command
//! buffers, the dynamic geometry buffers and the staging buffer used for texture
//! updates.
//!
//! Threading: every call into the API happens on the interface thread. The
//! capture and processing threads never touch it and hand their data over
//! through the queued upload path called from here, which keeps the queue usage
//! single threaded and removes the need for external synchronization on command
//! pools.
//!
//! Texture updates come in three forms.
//!
//! The immediate copy blocks on its own fence and is used only while loading.
//!
//! The queued copy appends the payload to a per frame staging buffer and records
//! the copy at the head of the frame command buffer, which is what the glyph
//! atlas and the waterfall use: no stall and no extra submission.
//!
//! The queued clear records a clear command instead. It exists because a whole
//! waterfall texture is megabytes, and clearing it through the copy path would
//! move those megabytes twice through host memory to write nothing but nought.

pub mod batch;
pub mod device;
pub mod frame;
pub mod instance;
pub mod memory;
pub mod pipeline;
pub mod shaders;
pub mod surface;
pub mod swapchain;
pub mod texture;
pub mod vk;

use std::ffi::c_void;

use crate::config::settings::{PresentModeCfg, RenderSettings};
use crate::core::{Error, Result};

pub use batch::{Color, DrawList, Mode, Rect};
pub use texture::TextureId;

use device::Device;
use frame::Frame;
use instance::Instance;
use pipeline::{UiPipeline, PUSH_BYTES};
use surface::Surface;
use swapchain::Swapchain;
use texture::TextureStore;
use vk::*;

/// Initial size of the per frame staging buffer.
///
/// It grows on demand; this covers a full glyph atlas row plus several waterfall
/// lines, which is the ordinary per frame volume.
const STAGING_INITIAL_BYTES: usize = 256 * 1024;

/// Entries in the palette texture bound at the auxiliary slot.
const PALETTE_ENTRIES: u32 = 256;

/// Frames the device side peak is held over before it is republished.
///
/// Sixty four is about a second at the ordinary rate, which matches the interval
/// the processor side clock measures over so the two readings describe comparable
/// windows.
const GPU_PEAK_WINDOW: u32 = 64;

#[derive(Debug, Clone, Copy, Default)]
pub struct RenderStats {
    pub draw_calls: u32,
    pub vertices: u32,
    pub indices: u32,
    pub uploads: u32,
    pub upload_bytes: u32,
    /// Whole texture clears recorded this frame.
    ///
    /// Counted apart from the copies because they carry no bytes: a clear that
    /// showed up as a nought byte upload would make the upload figure look like
    /// a fault.
    pub clears: u32,
    /// Device side frame time in milliseconds, nought when not measured.
    ///
    /// One frame behind the processor side figure, because a query can only be
    /// read once the submission that wrote it has completed.
    pub gpu_ms: f32,
    /// Worst device side frame of the last window.
    pub gpu_worst_ms: f32,
    /// Frames dropped because the swapchain had to be rebuilt.
    pub skipped_frames: u64,
    pub swapchain_rebuilds: u64,
}

/// One texture operation waiting to be recorded at the head of the frame.
///
/// One list of both kinds rather than two lists, because the order matters: a
/// clear queued after a copy must not be recorded before it, and two lists would
/// lose that relation.
enum PendingWork {
    Copy {
        texture: TextureId,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        /// Byte offset inside the staging scratch, already aligned.
        offset: usize,
    },
    Clear {
        texture: TextureId,
    },
}

impl PendingWork {
    fn texture(&self) -> TextureId {
        match self {
            PendingWork::Copy { texture, .. } => *texture,
            PendingWork::Clear { texture } => *texture,
        }
    }
}

pub struct Renderer {
    instance: Instance,
    device: Device,
    surface: Surface,
    swapchain: Swapchain,
    pipeline: UiPipeline,
    textures: TextureStore,
    frames: Vec<Frame>,

    frame_index: usize,
    white: TextureId,
    /// Palette the waterfall shader samples, bound at the auxiliary slot.
    palette: TextureId,
    aux_set: VkDescriptorSet,
    /// Two queries per frame slot, or the null handle when not measuring.
    query_pool: VkQueryPool,
    /// False for a slot that has not been submitted since the pool was created,
    /// whose queries therefore hold nothing.
    slot_timed: Vec<bool>,
    gpu_ms: f32,
    gpu_peak_ms: f32,
    gpu_peak_count: u32,
    clear_color: [f32; 4],
    vsync: bool,
    present_pref: PresentModeCfg,
    needs_recreate: bool,
    size: (u32, u32),
    stats: RenderStats,

    pending: Vec<PendingWork>,
    upload_scratch: Vec<u8>,
}

impl Renderer {
    pub fn new(
        hwnd: *mut c_void,
        hinstance: *mut c_void,
        size: (u32, u32),
        vsync: bool,
        cfg: &RenderSettings,
    ) -> Result<Renderer> {
        let instance = Instance::new(cfg.validation)?;
        let surface = Surface::new(&instance, hwnd, hinstance)?;
        let device = Device::new(&instance, &surface, cfg)?;

        let swapchain = Swapchain::new(
            &instance,
            &device,
            &surface,
            size,
            vsync,
            cfg.present_mode,
            cfg.swapchain_images,
            VK_NULL_HANDLE,
        )?;

        let pipeline = UiPipeline::new(&device, swapchain.render_pass)?;
        let mut textures =
            TextureStore::new(&device, pipeline.descriptor_layout, cfg.max_textures)?;

        // The palette is created before anything else and bound for the whole
        // session. The fragment shader names it statically, so a set must be
        // bound whether or not the waterfall is drawn in a given frame.
        //
        // Linear filtering rather than nearest: the shader samples it at a
        // continuous coordinate, and nearest would quantize a smooth gradient to
        // the two hundred and fifty six entries the table happens to hold.
        let flat = vec![0u8; PALETTE_ENTRIES as usize * 4];
        let palette = textures.create_rgba8(&device, PALETTE_ENTRIES, 1, &flat, false)?;
        let aux_set = textures.create_aux_set(&device, pipeline.aux_layout, palette, false)?;

        // A single opaque white texel keeps the descriptor set valid for solid
        // geometry, so the pipeline never has to be switched.
        let white = textures.create_rgba8(&device, 1, 1, &[255, 255, 255, 255], false)?;

        let mut frames = Vec::with_capacity(cfg.frames_in_flight as usize);
        for slot in 0..cfg.frames_in_flight {
            frames.push(Frame::new(
                &device,
                cfg.vertex_buffer_kb as usize * 1024,
                cfg.index_buffer_kb as usize * 1024,
                STAGING_INITIAL_BYTES,
                slot,
            )?);
        }

        // The pool is created after the frames because its size follows their
        // count. Two queries per slot rather than two overall: several slots are
        // in flight at once, and one pair would be overwritten by the next frame
        // before the previous one could be read.
        let mut query_pool: VkQueryPool = VK_NULL_HANDLE;
        if cfg.gpu_timing {
            if device.supports_timestamps() {
                let info = VkQueryPoolCreateInfo {
                    queryType: VK_QUERY_TYPE_TIMESTAMP,
                    queryCount: frames.len() as u32 * 2,
                    ..Default::default()
                };
                check("vkCreateQueryPool", unsafe {
                    (device.fns.create_query_pool)(
                        device.handle,
                        &info,
                        NO_ALLOCATOR,
                        &mut query_pool,
                    )
                })?;
                crate::log_info!(
                    "render",
                    "device timing enabled, {:.1} ns per tick, {} valid bits",
                    device.timestamp_period_ns,
                    device.timestamp_valid_bits
                );
            } else {
                crate::log_info!(
                    "render",
                    "device timing requested but the graphics queue reports no timestamp bits"
                );
            }
        }
        let slot_timed = vec![false; frames.len()];

        // The swapchain may be created with an sRGB format, in which case the
        // clear value has to be supplied in linear space to match the palette
        // the geometry carries.
        let bg = cfg.background_rgb;
        let clear_color = [
            srgb_to_linear(((bg >> 16) & 0xFF) as f32 / 255.0),
            srgb_to_linear(((bg >> 8) & 0xFF) as f32 / 255.0),
            srgb_to_linear((bg & 0xFF) as f32 / 255.0),
            1.0,
        ];

        crate::log_info!(
            "render",
            "ready: {} images, {} frames in flight, format {}, present {}",
            swapchain.images.len(),
            frames.len(),
            format_name(swapchain.format),
            present_mode_name(swapchain.present_mode)
        );

        Ok(Renderer {
            instance,
            device,
            surface,
            swapchain,
            pipeline,
            textures,
            frames,
            frame_index: 0,
            white,
            palette,
            aux_set,
            query_pool,
            slot_timed,
            gpu_ms: 0.0,
            gpu_peak_ms: 0.0,
            gpu_peak_count: 0,
            clear_color,
            vsync,
            present_pref: cfg.present_mode,
            needs_recreate: false,
            size,
            stats: RenderStats::default(),
            pending: Vec::with_capacity(16),
            upload_scratch: Vec::with_capacity(STAGING_INITIAL_BYTES),
        })
    }

    pub fn white_texture(&self) -> TextureId {
        self.white
    }

    /// Palette the waterfall shader samples.
    ///
    /// Exposed so the owner of the colour table can upload into it through the
    /// ordinary queued path rather than through an interface of its own.
    pub fn palette_texture(&self) -> TextureId {
        self.palette
    }

    pub fn stats(&self) -> RenderStats {
        let mut out = self.stats;
        out.gpu_ms = self.gpu_ms;
        out.gpu_worst_ms = self.gpu_worst_ms();
        out
    }

    /// Worst device side frame of the last completed window.
    ///
    /// The window in progress is reported when it already holds a higher value,
    /// so a single stall is visible on the frame after it happened rather than up
    /// to a second later.
    fn gpu_worst_ms(&self) -> f32 {
        self.gpu_peak_ms.max(self.stats.gpu_worst_ms)
    }

    pub fn surface_size(&self) -> (u32, u32) {
        (self.swapchain.extent.width, self.swapchain.extent.height)
    }

    pub fn device_name(&self) -> &str {
        &self.device.name
    }

    /// Records the new client size.
    ///
    /// The swapchain is rebuilt lazily on the next frame, which coalesces the
    /// message storm a resize drag produces into one rebuild.
    pub fn resize(&mut self, width: u32, height: u32) {
        if (width, height) != self.size {
            self.size = (width, height);
            self.needs_recreate = true;
        }
    }

    pub fn set_vsync(&mut self, on: bool) {
        if self.vsync != on {
            self.vsync = on;
            self.needs_recreate = true;
        }
    }

    pub fn create_texture_rgba8(
        &mut self,
        w: u32,
        h: u32,
        data: &[u8],
        nearest: bool,
    ) -> Result<TextureId> {
        self.textures.create_rgba8(&self.device, w, h, data, nearest)
    }

    pub fn create_texture_r8(
        &mut self,
        w: u32,
        h: u32,
        data: &[u8],
        nearest: bool,
    ) -> Result<TextureId> {
        self.textures.create_r8(&self.device, w, h, data, nearest)
    }

    /// Immediate update, blocking until the copy is done.
    pub fn update_texture(
        &mut self,
        id: TextureId,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        data: &[u8],
    ) -> Result<()> {
        self.textures.update_region(&self.device, id, x, y, w, h, data)
    }

    /// Changes how a texture is sampled.
    ///
    /// Drains the device first, because the descriptor set may be referenced by
    /// a submission still in flight and updating one that is would be reading a
    /// sampler while it is replaced. The drain is why this is not called per
    /// frame: it is a response to an operator switch, which happens once.
    pub fn set_texture_filter(&mut self, id: TextureId, nearest: bool) -> Result<()> {
        if self.textures.info(id).is_none() {
            return Err(Error::vulkan("set_texture_filter on an unknown texture"));
        }
        // Asked before draining, so a call that changes nothing costs nothing.
        if !self.textures.would_change_filter(id, nearest) {
            return Ok(());
        }
        self.device.wait_idle()?;
        self.textures.set_filter(&self.device, id, nearest);
        Ok(())
    }

    /// Queues a region for the next submitted frame.
    pub fn queue_texture_update(
        &mut self,
        id: TextureId,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        data: &[u8],
    ) -> Result<()> {
        if w == 0 || h == 0 {
            return Ok(());
        }
        let (_, extent, bytes_per_pixel, _) = self
            .textures
            .info(id)
            .ok_or_else(|| Error::vulkan("queue_texture_update on an unknown texture"))?;

        let needed = (w as usize) * (h as usize) * (bytes_per_pixel as usize);
        if data.len() < needed {
            return Err(Error::vulkan("queued update is shorter than the region"));
        }
        if x + w > extent.width || y + h > extent.height {
            return Err(Error::vulkan("queued update leaves the texture"));
        }

        // The copy offset must be a multiple of four and of the texel size.
        // Sixteen satisfies both for every format the store can hold, so one
        // alignment covers every case rather than one per format.
        let offset = (self.upload_scratch.len() + 15) & !15;
        self.upload_scratch.resize(offset, 0);
        self.upload_scratch.extend_from_slice(&data[..needed]);
        self.pending.push(PendingWork::Copy { texture: id, x, y, w, h, offset });
        Ok(())
    }

    /// Queues a whole texture clear for the next submitted frame.
    ///
    /// The clear covers the entire subresource, which is why there is no partial
    /// form: a strip is cleared by uploading zeros, and a whole texture is not,
    /// because a whole waterfall is several megabytes and the upload route would
    /// pass all of them twice through host memory.
    pub fn queue_texture_clear(&mut self, id: TextureId) -> Result<()> {
        if self.textures.info(id).is_none() {
            return Err(Error::vulkan("queue_texture_clear on an unknown texture"));
        }
        self.pending.push(PendingWork::Clear { texture: id });
        Ok(())
    }

    /// Reads the queries the previous use of this slot wrote.
    ///
    /// Called after the frame fence has been waited on, which is what makes the
    /// results available: the fence signals only once the submission that wrote
    /// them has completed, so no wait flag is needed on the query itself.
    fn read_timing(&mut self, slot: usize) {
        if self.query_pool == VK_NULL_HANDLE || !self.slot_timed[slot] {
            return;
        }

        let base = slot as u32 * 2;
        let mut values = [0u64; 2];
        let r = unsafe {
            (self.device.fns.get_query_pool_results)(
                self.device.handle,
                self.query_pool,
                base,
                2,
                std::mem::size_of_val(&values),
                values.as_mut_ptr() as *mut c_void,
                std::mem::size_of::<u64>() as VkDeviceSize,
                VK_QUERY_RESULT_64_BIT,
            )
        };
        if r != VK_SUCCESS {
            // The fence has signalled, so this is an implementation declining to
            // report rather than work still in flight. The sample is dropped
            // rather than recorded as nought, which would read as a device that
            // finished the frame instantly.
            return;
        }

        let begin = self.device.mask_timestamp(values[0]);
        let end = self.device.mask_timestamp(values[1]);
        let ticks = self.device.mask_timestamp(end.wrapping_sub(begin));
        let ms = ticks as f64 * self.device.timestamp_period_ns as f64 / 1.0e6;
        self.gpu_ms = ms as f32;

        if self.gpu_ms > self.gpu_peak_ms {
            self.gpu_peak_ms = self.gpu_ms;
        }
        self.gpu_peak_count += 1;
        if self.gpu_peak_count >= GPU_PEAK_WINDOW {
            self.stats.gpu_worst_ms = self.gpu_peak_ms;
            self.gpu_peak_ms = 0.0;
            self.gpu_peak_count = 0;
        }
    }

    /// Submits one frame.
    ///
    /// Returns false when the frame was skipped, which happens while minimized
    /// and immediately after a swapchain rebuild. Pending work stays queued in
    /// that case, so nothing is lost.
    pub fn render(&mut self, list: &DrawList) -> Result<bool> {
        if self.size.0 == 0 || self.size.1 == 0 {
            return Ok(false);
        }
        if self.needs_recreate {
            self.recreate_swapchain()?;
            if self.swapchain.extent.width == 0 || self.swapchain.extent.height == 0 {
                return Ok(false);
            }
        }

        let slot = self.frame_index;

        // The frame slot must be free before its buffers are touched.
        check("vkWaitForFences", unsafe {
            (self.device.fns.wait_for_fences)(
                self.device.handle,
                1,
                &self.frames[slot].in_flight,
                VK_TRUE,
                u64::MAX,
            )
        })?;

        // Read before anything overwrites the slot. The pool is reset inside the
        // command buffer recorded below, so the values belong to the previous use
        // of this slot and are gone once recording begins.
        self.read_timing(slot);

        let mut image_index: u32 = 0;
        let acquired = unsafe {
            (self.device.fns.acquire_next_image_khr)(
                self.device.handle,
                self.swapchain.handle,
                u64::MAX,
                self.frames[slot].image_available,
                VK_NULL_HANDLE,
                &mut image_index,
            )
        };
        match acquired {
            VK_SUCCESS => {}
            VK_SUBOPTIMAL_KHR => {
                // The image is still presentable, so the frame proceeds and the
                // rebuild happens on the next one.
                self.needs_recreate = true;
            }
            VK_ERROR_OUT_OF_DATE_KHR => {
                self.needs_recreate = true;
                self.stats.skipped_frames += 1;
                return Ok(false);
            }
            other => return Err(vk_err("vkAcquireNextImageKHR", other)),
        }

        // Reset only after a successful acquire: an early return would otherwise
        // leave the fence unsignalled and the next wait would never return.
        check("vkResetFences", unsafe {
            (self.device.fns.reset_fences)(self.device.handle, 1, &self.frames[slot].in_flight)
        })?;

        {
            let Renderer { device, frames, .. } = self;
            frames[slot].upload(device, list)?;
        }
        {
            let Renderer { device, frames, upload_scratch, .. } = self;
            frames[slot].upload_staging(device, upload_scratch)?;
        }

        self.record(slot, image_index as usize, list)?;

        self.stats.uploads = 0;
        self.stats.clears = 0;
        self.stats.upload_bytes = self.upload_scratch.len() as u32;
        for work in self.pending.drain(..) {
            match work {
                PendingWork::Copy { .. } => self.stats.uploads += 1,
                PendingWork::Clear { .. } => self.stats.clears += 1,
            }
            // Both recorded operations leave the image in the sampled layout, so
            // the store has to be told or the next upload would declare a stale
            // source scope.
            self.textures.note_uploaded(work.texture());
        }
        self.upload_scratch.clear();

        let wait = [self.frames[slot].image_available];
        let stages = [VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT];
        let buffers = [self.frames[slot].command_buffer];
        // The signal semaphore is per image rather than per frame slot: a
        // presentation engine may hold an image longer than the slot lifetime.
        let signal = [self.swapchain.render_finished[image_index as usize]];

        let submit = VkSubmitInfo {
            waitSemaphoreCount: wait.len() as u32,
            pWaitSemaphores: wait.as_ptr(),
            pWaitDstStageMask: stages.as_ptr(),
            commandBufferCount: buffers.len() as u32,
            pCommandBuffers: buffers.as_ptr(),
            signalSemaphoreCount: signal.len() as u32,
            pSignalSemaphores: signal.as_ptr(),
            ..Default::default()
        };

        check("vkQueueSubmit", unsafe {
            (self.device.fns.queue_submit)(
                self.device.graphics_queue,
                1,
                &submit,
                self.frames[slot].in_flight,
            )
        })?;

        if self.query_pool != VK_NULL_HANDLE {
            self.slot_timed[slot] = true;
        }

        let swapchains = [self.swapchain.handle];
        let indices = [image_index];
        let present = VkPresentInfoKHR {
            waitSemaphoreCount: signal.len() as u32,
            pWaitSemaphores: signal.as_ptr(),
            swapchainCount: swapchains.len() as u32,
            pSwapchains: swapchains.as_ptr(),
            pImageIndices: indices.as_ptr(),
            ..Default::default()
        };

        let presented =
            unsafe { (self.device.fns.queue_present_khr)(self.device.present_queue, &present) };
        match presented {
            VK_SUCCESS => {}
            VK_SUBOPTIMAL_KHR | VK_ERROR_OUT_OF_DATE_KHR => self.needs_recreate = true,
            other => return Err(vk_err("vkQueuePresentKHR", other)),
        }

        self.stats.draw_calls = list.commands.len() as u32;
        self.stats.vertices = list.vertices.len() as u32;
        self.stats.indices = list.indices.len() as u32;
        self.frame_index = (self.frame_index + 1) % self.frames.len();
        Ok(true)
    }

    fn record(&self, slot: usize, image_index: usize, list: &DrawList) -> Result<()> {
        let device = &self.device;
        let frame = &self.frames[slot];
        let cb = frame.command_buffer;
        let extent = self.swapchain.extent;

        check("vkResetCommandPool", unsafe {
            (device.fns.reset_command_pool)(device.handle, frame.command_pool, 0)
        })?;

        let begin = VkCommandBufferBeginInfo {
            flags: VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
            ..Default::default()
        };
        check("vkBeginCommandBuffer", unsafe {
            (device.fns.begin_command_buffer)(cb, &begin)
        })?;

        // The reset covers this slot alone. Resetting the whole pool would
        // discard the queries of the slots still in flight, which are read on the
        // frame after they complete.
        if self.query_pool != VK_NULL_HANDLE {
            let base = slot as u32 * 2;
            unsafe {
                (device.fns.cmd_reset_query_pool)(cb, self.query_pool, base, 2);
                (device.fns.cmd_write_timestamp)(
                    cb,
                    VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,
                    self.query_pool,
                    base,
                );
            }
        }

        unsafe {
            // Texture work goes first: the draw calls below may sample the rows
            // being written, and an image layout transition is not legal inside
            // a render pass.
            self.record_pending(cb, frame);

            let clear = VkClearValue { color: self.clear_color };
            let rp_begin = VkRenderPassBeginInfo {
                renderPass: self.swapchain.render_pass,
                framebuffer: self.swapchain.framebuffers[image_index],
                renderArea: VkRect2D {
                    offset: VkOffset2D { x: 0, y: 0 },
                    extent,
                },
                clearValueCount: 1,
                pClearValues: &clear,
                ..Default::default()
            };
            (device.fns.cmd_begin_render_pass)(cb, &rp_begin, VK_SUBPASS_CONTENTS_INLINE);

            (device.fns.cmd_bind_pipeline)(
                cb,
                VK_PIPELINE_BIND_POINT_GRAPHICS,
                self.pipeline.pipeline,
            );

            // The auxiliary set is bound once and never rebound. The fragment
            // shader names it statically, so it must be valid on every draw
            // whether or not that draw samples it.
            (device.fns.cmd_bind_descriptor_sets)(
                cb,
                VK_PIPELINE_BIND_POINT_GRAPHICS,
                self.pipeline.layout,
                1,
                1,
                &self.aux_set,
                0,
                std::ptr::null(),
            );

            let viewport = VkViewport {
                x: 0.0,
                y: 0.0,
                width: extent.width as f32,
                height: extent.height as f32,
                minDepth: 0.0,
                maxDepth: 1.0,
            };
            (device.fns.cmd_set_viewport)(cb, 0, 1, &viewport);

            // Six floats: the pixel to clip conversion factor, then the
            // waterfall mapping. Set once per frame because there is one
            // waterfall and it is drawn once.
            let push: [f32; 6] = [
                extent.width as f32,
                extent.height as f32,
                list.waterfall_store_span_db,
                list.waterfall_display_span_db,
                list.waterfall_inv_gamma,
                0.0,
            ];
            (device.fns.cmd_push_constants)(
                cb,
                self.pipeline.layout,
                VK_SHADER_STAGE_VERTEX_BIT | VK_SHADER_STAGE_FRAGMENT_BIT,
                0,
                PUSH_BYTES,
                push.as_ptr() as *const c_void,
            );

            if !list.indices.is_empty() {
                let vertex_buffers = [frame.vertex.buffer.handle];
                let offsets: [VkDeviceSize; 1] = [0];
                (device.fns.cmd_bind_vertex_buffers)(
                    cb,
                    0,
                    1,
                    vertex_buffers.as_ptr(),
                    offsets.as_ptr(),
                );
                (device.fns.cmd_bind_index_buffer)(
                    cb,
                    frame.index.buffer.handle,
                    0,
                    VK_INDEX_TYPE_UINT32,
                );

                let mut bound = TextureId::INVALID;
                for cmd in &list.commands {
                    if cmd.index_count == 0 {
                        continue;
                    }

                    // The scissor is clamped to the framebuffer: a rectangle
                    // that leaves the render area is rejected outright.
                    let x = cmd.clip[0].max(0).min(extent.width as i32);
                    let y = cmd.clip[1].max(0).min(extent.height as i32);
                    let w = (cmd.clip[2].min(extent.width as i32) - x).max(0);
                    let h = (cmd.clip[3].min(extent.height as i32) - y).max(0);
                    if w == 0 || h == 0 {
                        continue;
                    }
                    let scissor = VkRect2D {
                        offset: VkOffset2D { x, y },
                        extent: VkExtent2D { width: w as u32, height: h as u32 },
                    };
                    (device.fns.cmd_set_scissor)(cb, 0, 1, &scissor);

                    if cmd.texture != bound {
                        let set = self.textures.descriptor(cmd.texture).unwrap_or_else(|| {
                            self.textures
                                .descriptor(self.white)
                                .expect("the white texture is missing")
                        });
                        (device.fns.cmd_bind_descriptor_sets)(
                            cb,
                            VK_PIPELINE_BIND_POINT_GRAPHICS,
                            self.pipeline.layout,
                            0,
                            1,
                            &set,
                            0,
                            std::ptr::null(),
                        );
                        bound = cmd.texture;
                    }

                    (device.fns.cmd_draw_indexed)(cb, cmd.index_count, 1, cmd.index_offset, 0, 0);
                }
            }

            (device.fns.cmd_end_render_pass)(cb);

            // At the bottom of the pipeline rather than the top, so the interval
            // covers the work rather than only the command submission.
            if self.query_pool != VK_NULL_HANDLE {
                (device.fns.cmd_write_timestamp)(
                    cb,
                    VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT,
                    self.query_pool,
                    slot as u32 * 2 + 1,
                );
            }
        }

        check("vkEndCommandBuffer", unsafe {
            (device.fns.end_command_buffer)(cb)
        })?;
        Ok(())
    }

    /// Emits the barrier, operation and barrier triple for every queued item.
    ///
    /// The old layout is the one the texture already holds, so the contents
    /// outside the region survive; the first barrier also carries the execution
    /// dependency against the sampling done by earlier frames.
    unsafe fn record_pending(&self, cb: VkCommandBuffer, frame: &Frame) {
        if self.pending.is_empty() {
            return;
        }
        let device = &self.device;
        let staging = frame.staging.buffer.handle;

        let range = VkImageSubresourceRange {
            aspectMask: VK_IMAGE_ASPECT_COLOR_BIT,
            baseMipLevel: 0,
            levelCount: 1,
            baseArrayLayer: 0,
            layerCount: 1,
        };

        for work in &self.pending {
            let (image, _, _, layout) = match self.textures.info(work.texture()) {
                Some(v) => v,
                None => continue,
            };
            let (src_stage, src_access) = if layout == VK_IMAGE_LAYOUT_UNDEFINED {
                (VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, 0)
            } else {
                (VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, VK_ACCESS_SHADER_READ_BIT)
            };

            let to_dst = VkImageMemoryBarrier {
                srcAccessMask: src_access,
                dstAccessMask: VK_ACCESS_TRANSFER_WRITE_BIT,
                oldLayout: layout,
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

            match work {
                PendingWork::Copy { x, y, w, h, offset, .. } => {
                    // A zero row length and image height mean the payload is
                    // tightly packed for the region rather than for the full
                    // texture width.
                    let region = VkBufferImageCopy {
                        bufferOffset: *offset as VkDeviceSize,
                        bufferRowLength: 0,
                        bufferImageHeight: 0,
                        imageSubresource: VkImageSubresourceLayers {
                            aspectMask: VK_IMAGE_ASPECT_COLOR_BIT,
                            mipLevel: 0,
                            baseArrayLayer: 0,
                            layerCount: 1,
                        },
                        imageOffset: VkOffset3D { x: *x as i32, y: *y as i32, z: 0 },
                        imageExtent: VkExtent3D { width: *w, height: *h, depth: 1 },
                    };
                    (device.fns.cmd_copy_buffer_to_image)(
                        cb,
                        staging,
                        image,
                        VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                        1,
                        &region,
                    );
                }
                PendingWork::Clear { .. } => {
                    // Nought reads as the bottom of the palette in the mapped
                    // arrangement and as transparent black in the direct one, so
                    // one value serves both.
                    let value: VkClearColorValue = [0.0, 0.0, 0.0, 0.0];
                    (device.fns.cmd_clear_color_image)(
                        cb,
                        image,
                        VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                        &value,
                        1,
                        &range,
                    );
                }
            }

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
        }
    }

    fn recreate_swapchain(&mut self) -> Result<()> {
        self.device.wait_idle()?;

        let old = self.swapchain.handle;
        let next = Swapchain::new(
            &self.instance,
            &self.device,
            &self.surface,
            self.size,
            self.vsync,
            self.present_pref,
            0,
            old,
        )?;

        // The old handle was consumed as the old swapchain, so only the
        // dependent objects are released here; destroying it before the images
        // were transferred would invalidate them.
        self.swapchain.destroy_dependents(&self.device);
        if old != VK_NULL_HANDLE {
            unsafe {
                (self.device.fns.destroy_swapchain_khr)(self.device.handle, old, NO_ALLOCATOR);
            }
        }
        self.swapchain = next;
        self.needs_recreate = false;
        self.stats.swapchain_rebuilds += 1;

        crate::log_debug!(
            "render",
            "swapchain rebuilt {}x{} present {}",
            self.swapchain.extent.width,
            self.swapchain.extent.height,
            present_mode_name(self.swapchain.present_mode)
        );
        Ok(())
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // Every object below may still be referenced by in flight work, so the
        // device is drained first and the failure is ignored: there is nothing
        // left to abort into.
        let _ = self.device.wait_idle();

        if self.query_pool != VK_NULL_HANDLE {
            unsafe {
                (self.device.fns.destroy_query_pool)(
                    self.device.handle,
                    self.query_pool,
                    NO_ALLOCATOR,
                );
            }
            self.query_pool = VK_NULL_HANDLE;
        }

        for frame in self.frames.drain(..) {
            frame.destroy(&self.device);
        }
        self.textures.destroy(&self.device);
        self.pipeline.destroy(&self.device);
        self.swapchain.destroy(&self.device);
        self.device.destroy();
        self.surface.destroy(&self.instance);
        self.instance.destroy();
        crate::log_info!("render", "renderer destroyed");
    }
}

/// Converts one sRGB channel to linear.
///
/// Needed because the swapchain may present an sRGB format, which applies the
/// transfer curve on write; the clear value would otherwise be lighter than the
/// geometry drawn in the same colour.
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}