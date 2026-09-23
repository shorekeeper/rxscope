//! Graphics pipeline for all two dimensional interface geometry.
//!
//! One pipeline covers solid fills, glyphs, images and the waterfall; the
//! fragment shader branches on a per vertex mode. The viewport and the scissor
//! are dynamic so a resize does not force a rebuild.
//!
//! Two descriptor sets rather than one. The first is bound per draw command and
//! carries whichever texture the command names. The second is bound once per
//! frame and carries the waterfall palette, which is the same for every command
//! and would otherwise have to be written into the set of every glyph atlas.

use std::ffi::c_char;

use crate::core::Result;
use crate::render::batch::Vertex;
use crate::render::device::Device;
use crate::render::shaders;
use crate::render::vk::*;

/// Size of the push constant block, in bytes.
///
/// Six floats: the viewport and the four values the waterfall mapping needs.
/// Well inside the hundred and twenty eight bytes the specification guarantees,
/// so no limit has to be consulted.
pub const PUSH_BYTES: u32 = 24;

pub struct UiPipeline {
    pub pipeline: VkPipeline,
    pub layout: VkPipelineLayout,
    /// Set bound per draw command.
    pub descriptor_layout: VkDescriptorSetLayout,
    /// Set bound once per frame.
    pub aux_layout: VkDescriptorSetLayout,
}

impl UiPipeline {
    pub fn new(device: &Device, render_pass: VkRenderPass) -> Result<UiPipeline> {
        let vert = create_module(device, shaders::UI_VERT)?;
        let frag = create_module(device, shaders::UI_FRAG)?;

        // The modules are released on every exit path, including the failing
        // ones, so the body is a separate call rather than a chain of guards.
        let result = Self::build(device, render_pass, vert, frag);

        unsafe {
            (device.fns.destroy_shader_module)(device.handle, vert, NO_ALLOCATOR);
            (device.fns.destroy_shader_module)(device.handle, frag, NO_ALLOCATOR);
        }
        result
    }

    fn build(
        device: &Device,
        render_pass: VkRenderPass,
        vert: VkShaderModule,
        frag: VkShaderModule,
    ) -> Result<UiPipeline> {
        // Both sets hold one combined image sampler, so one description serves
        // for both layouts.
        let binding = VkDescriptorSetLayoutBinding {
            binding: 0,
            descriptorType: VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
            descriptorCount: 1,
            stageFlags: VK_SHADER_STAGE_FRAGMENT_BIT,
            pImmutableSamplers: std::ptr::null(),
        };
        let dsl_info = VkDescriptorSetLayoutCreateInfo {
            bindingCount: 1,
            pBindings: &binding,
            ..Default::default()
        };

        let mut descriptor_layout: VkDescriptorSetLayout = VK_NULL_HANDLE;
        check("vkCreateDescriptorSetLayout", unsafe {
            (device.fns.create_descriptor_set_layout)(
                device.handle,
                &dsl_info,
                NO_ALLOCATOR,
                &mut descriptor_layout,
            )
        })?;

        let mut aux_layout: VkDescriptorSetLayout = VK_NULL_HANDLE;
        check("vkCreateDescriptorSetLayout", unsafe {
            (device.fns.create_descriptor_set_layout)(
                device.handle,
                &dsl_info,
                NO_ALLOCATOR,
                &mut aux_layout,
            )
        })?;

        // The range covers both stages because the fragment shader reads the
        // waterfall mapping out of the same block the vertex shader reads the
        // viewport from, and a block declared in a stage the range excludes is
        // rejected outright.
        let push_range = VkPushConstantRange {
            stageFlags: VK_SHADER_STAGE_VERTEX_BIT | VK_SHADER_STAGE_FRAGMENT_BIT,
            offset: 0,
            size: PUSH_BYTES,
        };
        let set_layouts = [descriptor_layout, aux_layout];
        let layout_info = VkPipelineLayoutCreateInfo {
            setLayoutCount: set_layouts.len() as u32,
            pSetLayouts: set_layouts.as_ptr(),
            pushConstantRangeCount: 1,
            pPushConstantRanges: &push_range,
            ..Default::default()
        };
        let mut layout: VkPipelineLayout = VK_NULL_HANDLE;
        check("vkCreatePipelineLayout", unsafe {
            (device.fns.create_pipeline_layout)(
                device.handle,
                &layout_info,
                NO_ALLOCATOR,
                &mut layout,
            )
        })?;

        let entry = b"main\0".as_ptr() as *const c_char;
        let stages = [
            VkPipelineShaderStageCreateInfo {
                stage: VK_SHADER_STAGE_VERTEX_BIT,
                module: vert,
                pName: entry,
                ..Default::default()
            },
            VkPipelineShaderStageCreateInfo {
                stage: VK_SHADER_STAGE_FRAGMENT_BIT,
                module: frag,
                pName: entry,
                ..Default::default()
            },
        ];

        let binding_desc = VkVertexInputBindingDescription {
            binding: 0,
            stride: std::mem::size_of::<Vertex>() as u32,
            inputRate: VK_VERTEX_INPUT_RATE_VERTEX,
        };

        let attributes = [
            VkVertexInputAttributeDescription {
                location: 0,
                binding: 0,
                format: VK_FORMAT_R32G32_SFLOAT,
                offset: 0,
            },
            VkVertexInputAttributeDescription {
                location: 1,
                binding: 0,
                format: VK_FORMAT_R32G32_SFLOAT,
                offset: 8,
            },
            VkVertexInputAttributeDescription {
                location: 2,
                binding: 0,
                format: VK_FORMAT_R8G8B8A8_UNORM,
                offset: 16,
            },
            VkVertexInputAttributeDescription {
                location: 3,
                binding: 0,
                format: VK_FORMAT_R32_UINT,
                offset: 20,
            },
        ];

        let vertex_input = VkPipelineVertexInputStateCreateInfo {
            vertexBindingDescriptionCount: 1,
            pVertexBindingDescriptions: &binding_desc,
            vertexAttributeDescriptionCount: attributes.len() as u32,
            pVertexAttributeDescriptions: attributes.as_ptr(),
            ..Default::default()
        };

        let input_assembly = VkPipelineInputAssemblyStateCreateInfo {
            topology: VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST,
            primitiveRestartEnable: VK_FALSE,
            ..Default::default()
        };

        // The counts are required even though the values arrive through dynamic
        // state, because they declare how many of each the pipeline expects.
        let viewport_state = VkPipelineViewportStateCreateInfo {
            viewportCount: 1,
            scissorCount: 1,
            ..Default::default()
        };

        let raster = VkPipelineRasterizationStateCreateInfo {
            polygonMode: VK_POLYGON_MODE_FILL,
            cullMode: VK_CULL_MODE_NONE,
            frontFace: VK_FRONT_FACE_COUNTER_CLOCKWISE,
            lineWidth: 1.0,
            ..Default::default()
        };

        let multisample = VkPipelineMultisampleStateCreateInfo {
            rasterizationSamples: VK_SAMPLE_COUNT_1_BIT,
            ..Default::default()
        };

        // Straight alpha blending. The destination alpha is kept meaningful so a
        // future offscreen target composites correctly.
        let blend_attachment = VkPipelineColorBlendAttachmentState {
            blendEnable: VK_TRUE,
            srcColorBlendFactor: VK_BLEND_FACTOR_SRC_ALPHA,
            dstColorBlendFactor: VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
            colorBlendOp: VK_BLEND_OP_ADD,
            srcAlphaBlendFactor: VK_BLEND_FACTOR_ONE,
            dstAlphaBlendFactor: VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
            alphaBlendOp: VK_BLEND_OP_ADD,
            colorWriteMask: VK_COLOR_COMPONENT_RGBA,
        };
        let blend = VkPipelineColorBlendStateCreateInfo {
            attachmentCount: 1,
            pAttachments: &blend_attachment,
            ..Default::default()
        };

        let dynamic_states = [VK_DYNAMIC_STATE_VIEWPORT, VK_DYNAMIC_STATE_SCISSOR];
        let dynamic = VkPipelineDynamicStateCreateInfo {
            dynamicStateCount: dynamic_states.len() as u32,
            pDynamicStates: dynamic_states.as_ptr(),
            ..Default::default()
        };

        let info = VkGraphicsPipelineCreateInfo {
            stageCount: stages.len() as u32,
            pStages: stages.as_ptr(),
            pVertexInputState: &vertex_input,
            pInputAssemblyState: &input_assembly,
            pViewportState: &viewport_state,
            pRasterizationState: &raster,
            pMultisampleState: &multisample,
            pColorBlendState: &blend,
            pDynamicState: &dynamic,
            layout,
            renderPass: render_pass,
            subpass: 0,
            basePipelineIndex: -1,
            ..Default::default()
        };

        let mut pipeline: VkPipeline = VK_NULL_HANDLE;
        check("vkCreateGraphicsPipelines", unsafe {
            (device.fns.create_graphics_pipelines)(
                device.handle,
                VK_NULL_HANDLE,
                1,
                &info,
                NO_ALLOCATOR,
                &mut pipeline,
            )
        })?;

        Ok(UiPipeline { pipeline, layout, descriptor_layout, aux_layout })
    }

    pub fn destroy(&mut self, device: &Device) {
        unsafe {
            (device.fns.destroy_pipeline)(device.handle, self.pipeline, NO_ALLOCATOR);
            (device.fns.destroy_pipeline_layout)(device.handle, self.layout, NO_ALLOCATOR);
            (device.fns.destroy_descriptor_set_layout)(
                device.handle,
                self.descriptor_layout,
                NO_ALLOCATOR,
            );
            (device.fns.destroy_descriptor_set_layout)(
                device.handle,
                self.aux_layout,
                NO_ALLOCATOR,
            );
        }
        self.pipeline = VK_NULL_HANDLE;
        self.layout = VK_NULL_HANDLE;
        self.descriptor_layout = VK_NULL_HANDLE;
        self.aux_layout = VK_NULL_HANDLE;
    }
}

fn create_module(device: &Device, blob: &[u8]) -> Result<VkShaderModule> {
    let code = shaders::words(blob);
    let info = VkShaderModuleCreateInfo {
        codeSize: code.len() * 4,
        pCode: code.as_ptr(),
        ..Default::default()
    };
    let mut module: VkShaderModule = VK_NULL_HANDLE;
    check("vkCreateShaderModule", unsafe {
        (device.fns.create_shader_module)(device.handle, &info, NO_ALLOCATOR, &mut module)
    })?;
    Ok(module)
}