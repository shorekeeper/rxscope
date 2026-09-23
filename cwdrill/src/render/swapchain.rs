//! Swapchain, render pass and framebuffers.

use crate::config::settings::PresentModeCfg;
use crate::core::Result;
use crate::render::device::Device;
use crate::render::instance::Instance;
use crate::render::surface::Surface;
use crate::render::vk::*;

pub struct Swapchain {
    pub handle: VkSwapchainKHR,
    pub format: VkFormat,
    pub color_space: VkColorSpaceKHR,
    pub extent: VkExtent2D,
    pub present_mode: VkPresentModeKHR,
    pub images: Vec<VkImage>,
    pub views: Vec<VkImageView>,
    pub framebuffers: Vec<VkFramebuffer>,
    pub render_pass: VkRenderPass,
    /// One semaphore per image rather than per frame slot. A presentation
    /// engine may hold an image beyond the lifetime of the frame that produced
    /// it, so a per frame semaphore could be reused while still pending.
    pub render_finished: Vec<VkSemaphore>,
}

impl Swapchain {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        instance: &Instance,
        device: &Device,
        surface: &Surface,
        size: (u32, u32),
        vsync: bool,
        preference: PresentModeCfg,
        requested_images: u32,
        old: VkSwapchainKHR,
    ) -> Result<Swapchain> {
        let caps = surface.capabilities(instance, device.physical)?;
        let formats = surface.formats(instance, device.physical)?;
        let modes = surface.present_modes(instance, device.physical)?;

        let surface_format = pick_format(&formats);
        let present_mode = pick_present_mode(&modes, vsync, preference);

        // A current extent of the sentinel value means the surface lets the
        // application choose. Some drivers report it and some report the size
        // the compositor decided, so both paths have to exist.
        let extent = if caps.currentExtent.width != u32::MAX {
            caps.currentExtent
        } else {
            VkExtent2D {
                width: size
                    .0
                    .clamp(caps.minImageExtent.width, caps.maxImageExtent.width),
                height: size
                    .1
                    .clamp(caps.minImageExtent.height, caps.maxImageExtent.height),
            }
        };

        if extent.width == 0 || extent.height == 0 {
            // Minimized. An empty placeholder is returned so the caller can skip
            // rendering with a size test rather than by special casing an error.
            return Ok(Swapchain {
                handle: VK_NULL_HANDLE,
                format: surface_format.format,
                color_space: surface_format.colorSpace,
                extent,
                present_mode,
                images: Vec::new(),
                views: Vec::new(),
                framebuffers: Vec::new(),
                render_pass: VK_NULL_HANDLE,
                render_finished: Vec::new(),
            });
        }

        let mut image_count = if requested_images > 0 {
            requested_images
        } else {
            caps.minImageCount + 1
        };
        if caps.maxImageCount > 0 {
            image_count = image_count.min(caps.maxImageCount);
        }
        image_count = image_count.max(caps.minImageCount);

        let families = [device.graphics_family, device.present_family];
        let concurrent = device.graphics_family != device.present_family;

        let info = VkSwapchainCreateInfoKHR {
            surface: surface.handle,
            minImageCount: image_count,
            imageFormat: surface_format.format,
            imageColorSpace: surface_format.colorSpace,
            imageExtent: extent,
            imageArrayLayers: 1,
            imageUsage: VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT,
            imageSharingMode: if concurrent {
                VK_SHARING_MODE_CONCURRENT
            } else {
                VK_SHARING_MODE_EXCLUSIVE
            },
            queueFamilyIndexCount: if concurrent { families.len() as u32 } else { 0 },
            pQueueFamilyIndices: if concurrent {
                families.as_ptr()
            } else {
                std::ptr::null()
            },
            preTransform: caps.currentTransform,
            compositeAlpha: VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR,
            presentMode: present_mode,
            clipped: VK_TRUE,
            oldSwapchain: old,
            ..Default::default()
        };

        let mut handle: VkSwapchainKHR = VK_NULL_HANDLE;
        check("vkCreateSwapchainKHR", unsafe {
            (device.fns.create_swapchain_khr)(device.handle, &info, NO_ALLOCATOR, &mut handle)
        })?;

        let images = enumerate(|count, data| unsafe {
            (device.fns.get_swapchain_images_khr)(device.handle, handle, count, data)
        })
        .map_err(|r| vk_err("vkGetSwapchainImagesKHR", r))?;

        let render_pass = create_render_pass(device, surface_format.format)?;

        let mut views = Vec::with_capacity(images.len());
        let mut framebuffers = Vec::with_capacity(images.len());
        let mut render_finished = Vec::with_capacity(images.len());

        for &image in &images {
            let view_info = VkImageViewCreateInfo {
                image,
                viewType: VK_IMAGE_VIEW_TYPE_2D,
                format: surface_format.format,
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
                (device.fns.create_image_view)(
                    device.handle,
                    &view_info,
                    NO_ALLOCATOR,
                    &mut view,
                )
            })?;
            views.push(view);

            let fb_info = VkFramebufferCreateInfo {
                renderPass: render_pass,
                attachmentCount: 1,
                pAttachments: &view,
                width: extent.width,
                height: extent.height,
                layers: 1,
                ..Default::default()
            };
            let mut framebuffer: VkFramebuffer = VK_NULL_HANDLE;
            check("vkCreateFramebuffer", unsafe {
                (device.fns.create_framebuffer)(
                    device.handle,
                    &fb_info,
                    NO_ALLOCATOR,
                    &mut framebuffer,
                )
            })?;
            framebuffers.push(framebuffer);

            let sem_info = VkSemaphoreCreateInfo::default();
            let mut semaphore: VkSemaphore = VK_NULL_HANDLE;
            check("vkCreateSemaphore", unsafe {
                (device.fns.create_semaphore)(
                    device.handle,
                    &sem_info,
                    NO_ALLOCATOR,
                    &mut semaphore,
                )
            })?;
            render_finished.push(semaphore);
        }

        Ok(Swapchain {
            handle,
            format: surface_format.format,
            color_space: surface_format.colorSpace,
            extent,
            present_mode,
            images,
            views,
            framebuffers,
            render_pass,
            render_finished,
        })
    }

    /// Destroys everything except the swapchain handle.
    ///
    /// The handle is left because a rebuild passes it as the old swapchain,
    /// which transfers ownership of the images to the new one; destroying it
    /// here would invalidate them before the transfer.
    pub fn destroy_dependents(&mut self, device: &Device) {
        unsafe {
            for &semaphore in &self.render_finished {
                (device.fns.destroy_semaphore)(device.handle, semaphore, NO_ALLOCATOR);
            }
            for &framebuffer in &self.framebuffers {
                (device.fns.destroy_framebuffer)(device.handle, framebuffer, NO_ALLOCATOR);
            }
            for &view in &self.views {
                (device.fns.destroy_image_view)(device.handle, view, NO_ALLOCATOR);
            }
            if self.render_pass != VK_NULL_HANDLE {
                (device.fns.destroy_render_pass)(device.handle, self.render_pass, NO_ALLOCATOR);
            }
        }
        self.render_finished.clear();
        self.framebuffers.clear();
        self.views.clear();
        self.images.clear();
        self.render_pass = VK_NULL_HANDLE;
    }

    pub fn destroy(&mut self, device: &Device) {
        self.destroy_dependents(device);
        if self.handle != VK_NULL_HANDLE {
            unsafe {
                (device.fns.destroy_swapchain_khr)(device.handle, self.handle, NO_ALLOCATOR);
            }
            self.handle = VK_NULL_HANDLE;
        }
    }
}

/// Picks the presentation format.
///
/// An unsigned normalized format is preferred so the interface palette reaches
/// the display literally. An sRGB swapchain applies a transfer curve to every
/// flat colour, which would have to be undone in the shader for no gain.
fn pick_format(formats: &[VkSurfaceFormatKHR]) -> VkSurfaceFormatKHR {
    if formats.len() == 1 && formats[0].format == VK_FORMAT_UNDEFINED {
        // The single undefined entry means the surface accepts anything.
        return VkSurfaceFormatKHR {
            format: VK_FORMAT_B8G8R8A8_UNORM,
            colorSpace: VK_COLOR_SPACE_SRGB_NONLINEAR_KHR,
        };
    }
    for wanted in [VK_FORMAT_B8G8R8A8_UNORM, VK_FORMAT_R8G8B8A8_UNORM] {
        if let Some(found) = formats
            .iter()
            .find(|f| f.format == wanted && f.colorSpace == VK_COLOR_SPACE_SRGB_NONLINEAR_KHR)
        {
            return *found;
        }
    }
    formats[0]
}

fn pick_present_mode(
    modes: &[VkPresentModeKHR],
    vsync: bool,
    preference: PresentModeCfg,
) -> VkPresentModeKHR {
    let has = |m: VkPresentModeKHR| modes.contains(&m);

    let explicit = match preference {
        PresentModeCfg::Auto => None,
        PresentModeCfg::Fifo => Some(VK_PRESENT_MODE_FIFO_KHR),
        PresentModeCfg::FifoRelaxed => Some(VK_PRESENT_MODE_FIFO_RELAXED_KHR),
        PresentModeCfg::Mailbox => Some(VK_PRESENT_MODE_MAILBOX_KHR),
        PresentModeCfg::Immediate => Some(VK_PRESENT_MODE_IMMEDIATE_KHR),
    };
    if let Some(mode) = explicit {
        if has(mode) {
            return mode;
        }
        crate::log_warn!(
            "render",
            "present mode {} is unsupported, falling back",
            present_mode_name(mode)
        );
    }

    if vsync {
        // The only mode the specification guarantees.
        VK_PRESENT_MODE_FIFO_KHR
    } else if has(VK_PRESENT_MODE_MAILBOX_KHR) {
        VK_PRESENT_MODE_MAILBOX_KHR
    } else if has(VK_PRESENT_MODE_IMMEDIATE_KHR) {
        VK_PRESENT_MODE_IMMEDIATE_KHR
    } else {
        VK_PRESENT_MODE_FIFO_KHR
    }
}

/// One subpass, one colour attachment, cleared on load and left ready to
/// present.
///
/// The external dependency holds the colour write until the acquire semaphore
/// has been signalled. Without it the attachment could be written while the
/// presentation engine still owns the image.
fn create_render_pass(device: &Device, format: VkFormat) -> Result<VkRenderPass> {
    let attachment = VkAttachmentDescription {
        format,
        samples: VK_SAMPLE_COUNT_1_BIT,
        loadOp: VK_ATTACHMENT_LOAD_OP_CLEAR,
        storeOp: VK_ATTACHMENT_STORE_OP_STORE,
        stencilLoadOp: VK_ATTACHMENT_LOAD_OP_DONT_CARE,
        stencilStoreOp: VK_ATTACHMENT_STORE_OP_DONT_CARE,
        initialLayout: VK_IMAGE_LAYOUT_UNDEFINED,
        finalLayout: VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
        ..Default::default()
    };

    let color_ref = VkAttachmentReference {
        attachment: 0,
        layout: VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
    };

    let subpass = VkSubpassDescription {
        pipelineBindPoint: VK_PIPELINE_BIND_POINT_GRAPHICS,
        colorAttachmentCount: 1,
        pColorAttachments: &color_ref,
        ..Default::default()
    };

    let dependency = VkSubpassDependency {
        srcSubpass: VK_SUBPASS_EXTERNAL,
        dstSubpass: 0,
        srcStageMask: VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT,
        dstStageMask: VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT,
        srcAccessMask: 0,
        dstAccessMask: VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT,
        dependencyFlags: 0,
    };

    let info = VkRenderPassCreateInfo {
        attachmentCount: 1,
        pAttachments: &attachment,
        subpassCount: 1,
        pSubpasses: &subpass,
        dependencyCount: 1,
        pDependencies: &dependency,
        ..Default::default()
    };

    let mut render_pass: VkRenderPass = VK_NULL_HANDLE;
    check("vkCreateRenderPass", unsafe {
        (device.fns.create_render_pass)(device.handle, &info, NO_ALLOCATOR, &mut render_pass)
    })?;
    Ok(render_pass)
}