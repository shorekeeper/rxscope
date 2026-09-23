//! Layout verification.
//!
//! A field written at the wrong offset is not a compile error and rarely an
//! immediate crash: the driver reads a plausible value from the neighbouring
//! field and the failure surfaces later as a wrong format, a rejected create
//! info or a corrupted image. These assertions turn that class of defect into a
//! test failure on the machine that introduced it.
//!
//! The expected values are the sizes and offsets the specification headers
//! produce on a target with eight byte pointers, which covers every platform
//! this application is built for. They are literals rather than computed
//! expressions on purpose: a computed expectation would reproduce whatever
//! mistake the declaration contains and agree with it.

use std::mem::{align_of, offset_of, size_of};

use super::types::*;

#[test]
fn handles_are_the_expected_width() {
    // A dispatchable handle is a pointer to a dispatch table, so it must stay
    // pointer sized. A non dispatchable handle is defined as a 64 bit integer,
    // which is also correct on a 64 bit target where it happens to be a
    // pointer.
    assert_eq!(size_of::<VkInstance>(), size_of::<*mut std::ffi::c_void>());
    assert_eq!(size_of::<VkDevice>(), size_of::<*mut std::ffi::c_void>());
    assert_eq!(size_of::<VkCommandBuffer>(), size_of::<*mut std::ffi::c_void>());
    assert_eq!(size_of::<VkBuffer>(), 8);
    assert_eq!(size_of::<VkImage>(), 8);
    assert_eq!(size_of::<VkDeviceMemory>(), 8);
    assert_eq!(size_of::<VkSwapchainKHR>(), 8);
}

#[test]
fn small_value_types_match() {
    assert_eq!(size_of::<VkExtent2D>(), 8);
    assert_eq!(size_of::<VkExtent3D>(), 12);
    assert_eq!(size_of::<VkOffset2D>(), 8);
    assert_eq!(size_of::<VkOffset3D>(), 12);
    assert_eq!(size_of::<VkRect2D>(), 16);
    assert_eq!(size_of::<VkViewport>(), 24);
    assert_eq!(size_of::<VkComponentMapping>(), 16);
    assert_eq!(size_of::<VkImageSubresourceRange>(), 20);
    assert_eq!(size_of::<VkImageSubresourceLayers>(), 16);

    // The union is sixteen bytes because its widest member is four floats. The
    // depth stencil member is smaller and therefore fits.
    assert_eq!(size_of::<VkClearValue>(), 16);
}

#[test]
fn driver_written_structures_match() {
    assert_eq!(size_of::<VkExtensionProperties>(), 260);
    assert_eq!(size_of::<VkLayerProperties>(), 520);

    assert_eq!(size_of::<VkMemoryType>(), 8);
    assert_eq!(size_of::<VkMemoryHeap>(), 16);
    assert_eq!(size_of::<VkPhysicalDeviceMemoryProperties>(), 520);
    assert_eq!(offset_of!(VkPhysicalDeviceMemoryProperties, memoryTypes), 4);
    assert_eq!(offset_of!(VkPhysicalDeviceMemoryProperties, memoryHeapCount), 260);
    assert_eq!(offset_of!(VkPhysicalDeviceMemoryProperties, memoryHeaps), 264);

    assert_eq!(size_of::<VkQueueFamilyProperties>(), 24);
    assert_eq!(size_of::<VkMemoryRequirements>(), 24);
    assert_eq!(size_of::<VkSurfaceCapabilitiesKHR>(), 52);
    assert_eq!(size_of::<VkSurfaceFormatKHR>(), 8);
    assert_eq!(offset_of!(VkSurfaceCapabilitiesKHR, currentExtent), 8);
    assert_eq!(offset_of!(VkSurfaceCapabilitiesKHR, currentTransform), 40);
}

/// The properties structure carries an opaque limits member.
///
/// Its hundred and thirty limit fields and five sparse ones are replaced by
/// padding, because only the timestamp period is read and transcribing the rest
/// would add a hundred and thirty chances of a silent offset error. The
/// assertions below pin every offset that is read and confirm the margin.
#[test]
fn physical_device_properties_layout_matches() {
    assert_eq!(offset_of!(VkPhysicalDeviceProperties, apiVersion), 0);
    assert_eq!(offset_of!(VkPhysicalDeviceProperties, driverVersion), 4);
    assert_eq!(offset_of!(VkPhysicalDeviceProperties, vendorID), 8);
    assert_eq!(offset_of!(VkPhysicalDeviceProperties, deviceID), 12);
    assert_eq!(offset_of!(VkPhysicalDeviceProperties, deviceType), 16);
    assert_eq!(offset_of!(VkPhysicalDeviceProperties, deviceName), 20);
    assert_eq!(offset_of!(VkPhysicalDeviceProperties, pipelineCacheUUID), 276);

    // Seven hundred and twenty: the limits member begins at 296, because the
    // cache identifier ends at 292 and the member needs eight byte alignment,
    // and the field sits 424 bytes into it.
    assert_eq!(offset_of!(VkPhysicalDeviceProperties, timestampPeriod), 720);

    // The specification form is 824 bytes on this target. A declaration shorter
    // than that would let the driver write past the caller allocation, so the
    // test states the requirement rather than the exact size.
    assert!(
        size_of::<VkPhysicalDeviceProperties>() >= 824,
        "the opaque tail is too short at {} bytes",
        size_of::<VkPhysicalDeviceProperties>()
    );
    // Eight byte alignment matches the device size fields the real declaration
    // holds inside the region this one leaves opaque.
    assert_eq!(align_of::<VkPhysicalDeviceProperties>(), 8);
}

#[test]
fn create_infos_begin_with_the_structure_type() {
    // Every create info is read by the loader through its first field before
    // anything else is examined, so an offset other than nought here means the
    // structure is misidentified rather than misread.
    assert_eq!(offset_of!(VkInstanceCreateInfo, sType), 0);
    assert_eq!(offset_of!(VkDeviceCreateInfo, sType), 0);
    assert_eq!(offset_of!(VkSwapchainCreateInfoKHR, sType), 0);
    assert_eq!(offset_of!(VkImageCreateInfo, sType), 0);
    assert_eq!(offset_of!(VkGraphicsPipelineCreateInfo, sType), 0);
    assert_eq!(offset_of!(VkWriteDescriptorSet, sType), 0);

    // The next pointer follows on an eight byte boundary, which is what forces
    // the four bytes of padding the compiler inserts.
    assert_eq!(offset_of!(VkInstanceCreateInfo, pNext), 8);
    assert_eq!(offset_of!(VkImageMemoryBarrier, pNext), 8);
}

#[test]
fn creation_structures_match() {
    // Sizes alone are a weak check: a structure with the same members in the
    // wrong order often occupies the same space. The offsets below pin every
    // boundary where the compiler inserts padding, because that is where a
    // reordering hides.
    assert_eq!(size_of::<VkApplicationInfo>(), 48);
    assert_eq!(offset_of!(VkApplicationInfo, pApplicationName), 16);
    assert_eq!(offset_of!(VkApplicationInfo, pEngineName), 32);
    assert_eq!(offset_of!(VkApplicationInfo, apiVersion), 44);

    assert_eq!(size_of::<VkInstanceCreateInfo>(), 64);
    assert_eq!(offset_of!(VkInstanceCreateInfo, pApplicationInfo), 24);
    assert_eq!(offset_of!(VkInstanceCreateInfo, ppEnabledLayerNames), 40);
    assert_eq!(offset_of!(VkInstanceCreateInfo, ppEnabledExtensionNames), 56);

    // Forty rather than thirty two: the queue count ends at twenty eight and the
    // priority pointer needs eight byte alignment, so four bytes of padding
    // separate them.
    assert_eq!(size_of::<VkDeviceQueueCreateInfo>(), 40);
    assert_eq!(offset_of!(VkDeviceQueueCreateInfo, queueFamilyIndex), 20);
    assert_eq!(offset_of!(VkDeviceQueueCreateInfo, queueCount), 24);
    assert_eq!(offset_of!(VkDeviceQueueCreateInfo, pQueuePriorities), 32);

    assert_eq!(size_of::<VkDeviceCreateInfo>(), 72);
    assert_eq!(offset_of!(VkDeviceCreateInfo, pQueueCreateInfos), 24);
    assert_eq!(offset_of!(VkDeviceCreateInfo, ppEnabledExtensionNames), 56);
    assert_eq!(offset_of!(VkDeviceCreateInfo, pEnabledFeatures), 64);

    assert_eq!(size_of::<VkWin32SurfaceCreateInfoKHR>(), 40);
    assert_eq!(offset_of!(VkWin32SurfaceCreateInfoKHR, hinstance), 24);
    assert_eq!(offset_of!(VkWin32SurfaceCreateInfoKHR, hwnd), 32);

    assert_eq!(size_of::<VkSwapchainCreateInfoKHR>(), 104);
    assert_eq!(offset_of!(VkSwapchainCreateInfoKHR, surface), 24);
    assert_eq!(offset_of!(VkSwapchainCreateInfoKHR, imageExtent), 44);
    assert_eq!(offset_of!(VkSwapchainCreateInfoKHR, pQueueFamilyIndices), 72);
    assert_eq!(offset_of!(VkSwapchainCreateInfoKHR, oldSwapchain), 96);

    assert_eq!(size_of::<VkPresentInfoKHR>(), 64);
    assert_eq!(offset_of!(VkPresentInfoKHR, pWaitSemaphores), 24);
    assert_eq!(offset_of!(VkPresentInfoKHR, pSwapchains), 40);
    assert_eq!(offset_of!(VkPresentInfoKHR, pImageIndices), 48);

    assert_eq!(size_of::<VkImageCreateInfo>(), 88);
    assert_eq!(offset_of!(VkImageCreateInfo, extent), 28);
    assert_eq!(offset_of!(VkImageCreateInfo, pQueueFamilyIndices), 72);
    assert_eq!(offset_of!(VkImageCreateInfo, initialLayout), 80);

    assert_eq!(size_of::<VkImageViewCreateInfo>(), 80);
    assert_eq!(offset_of!(VkImageViewCreateInfo, image), 24);
    assert_eq!(offset_of!(VkImageViewCreateInfo, components), 40);
    assert_eq!(offset_of!(VkImageViewCreateInfo, subresourceRange), 56);

    assert_eq!(size_of::<VkBufferCreateInfo>(), 56);
    assert_eq!(offset_of!(VkBufferCreateInfo, size), 24);
    assert_eq!(offset_of!(VkBufferCreateInfo, pQueueFamilyIndices), 48);

    assert_eq!(size_of::<VkMemoryAllocateInfo>(), 32);
    assert_eq!(offset_of!(VkMemoryAllocateInfo, allocationSize), 16);
    assert_eq!(offset_of!(VkMemoryAllocateInfo, memoryTypeIndex), 24);

    assert_eq!(size_of::<VkMappedMemoryRange>(), 40);
    assert_eq!(offset_of!(VkMappedMemoryRange, offset), 24);
    assert_eq!(offset_of!(VkMappedMemoryRange, size), 32);

    assert_eq!(size_of::<VkSamplerCreateInfo>(), 80);
    assert_eq!(offset_of!(VkSamplerCreateInfo, addressModeU), 32);
    assert_eq!(offset_of!(VkSamplerCreateInfo, minLod), 64);
    assert_eq!(offset_of!(VkSamplerCreateInfo, unnormalizedCoordinates), 76);
}

#[test]
fn barrier_and_copy_match() {
    assert_eq!(size_of::<VkImageMemoryBarrier>(), 72);
    assert_eq!(offset_of!(VkImageMemoryBarrier, srcAccessMask), 16);
    assert_eq!(offset_of!(VkImageMemoryBarrier, oldLayout), 24);
    assert_eq!(offset_of!(VkImageMemoryBarrier, newLayout), 28);
    assert_eq!(offset_of!(VkImageMemoryBarrier, image), 40);
    assert_eq!(offset_of!(VkImageMemoryBarrier, subresourceRange), 48);

    assert_eq!(size_of::<VkBufferImageCopy>(), 56);
    assert_eq!(offset_of!(VkBufferImageCopy, imageSubresource), 16);
    assert_eq!(offset_of!(VkBufferImageCopy, imageOffset), 32);
    assert_eq!(offset_of!(VkBufferImageCopy, imageExtent), 44);
}

#[test]
fn render_pass_structures_match() {
    assert_eq!(size_of::<VkAttachmentDescription>(), 36);
    assert_eq!(size_of::<VkAttachmentReference>(), 8);
    assert_eq!(size_of::<VkSubpassDependency>(), 28);

    assert_eq!(size_of::<VkSubpassDescription>(), 72);
    assert_eq!(offset_of!(VkSubpassDescription, pInputAttachments), 16);
    assert_eq!(offset_of!(VkSubpassDescription, colorAttachmentCount), 24);
    assert_eq!(offset_of!(VkSubpassDescription, pColorAttachments), 32);
    assert_eq!(offset_of!(VkSubpassDescription, pDepthStencilAttachment), 48);

    assert_eq!(size_of::<VkRenderPassCreateInfo>(), 64);
    assert_eq!(size_of::<VkFramebufferCreateInfo>(), 64);
    assert_eq!(size_of::<VkRenderPassBeginInfo>(), 64);
    assert_eq!(offset_of!(VkRenderPassBeginInfo, renderArea), 32);
}

#[test]
fn pipeline_structures_match() {
    assert_eq!(size_of::<VkPushConstantRange>(), 12);
    assert_eq!(size_of::<VkVertexInputBindingDescription>(), 12);
    assert_eq!(size_of::<VkVertexInputAttributeDescription>(), 16);
    assert_eq!(size_of::<VkPipelineColorBlendAttachmentState>(), 32);

    assert_eq!(size_of::<VkPipelineLayoutCreateInfo>(), 48);
    assert_eq!(size_of::<VkShaderModuleCreateInfo>(), 40);
    assert_eq!(size_of::<VkPipelineShaderStageCreateInfo>(), 48);
    assert_eq!(size_of::<VkPipelineVertexInputStateCreateInfo>(), 48);
    assert_eq!(size_of::<VkPipelineInputAssemblyStateCreateInfo>(), 32);
    assert_eq!(size_of::<VkPipelineViewportStateCreateInfo>(), 48);
    assert_eq!(size_of::<VkPipelineRasterizationStateCreateInfo>(), 64);
    assert_eq!(size_of::<VkPipelineMultisampleStateCreateInfo>(), 48);
    assert_eq!(size_of::<VkPipelineColorBlendStateCreateInfo>(), 56);
    assert_eq!(size_of::<VkPipelineDynamicStateCreateInfo>(), 32);

    assert_eq!(size_of::<VkGraphicsPipelineCreateInfo>(), 144);
    assert_eq!(offset_of!(VkGraphicsPipelineCreateInfo, pStages), 24);
    assert_eq!(offset_of!(VkGraphicsPipelineCreateInfo, pVertexInputState), 32);
    assert_eq!(offset_of!(VkGraphicsPipelineCreateInfo, pViewportState), 56);
    assert_eq!(offset_of!(VkGraphicsPipelineCreateInfo, pColorBlendState), 88);
    assert_eq!(offset_of!(VkGraphicsPipelineCreateInfo, pDynamicState), 96);
    assert_eq!(offset_of!(VkGraphicsPipelineCreateInfo, layout), 104);
    assert_eq!(offset_of!(VkGraphicsPipelineCreateInfo, renderPass), 112);
    assert_eq!(offset_of!(VkGraphicsPipelineCreateInfo, subpass), 120);
}

#[test]
fn descriptor_structures_match() {
    assert_eq!(size_of::<VkDescriptorSetLayoutBinding>(), 24);
    assert_eq!(size_of::<VkDescriptorPoolSize>(), 8);
    assert_eq!(size_of::<VkDescriptorImageInfo>(), 24);

    assert_eq!(size_of::<VkDescriptorSetLayoutCreateInfo>(), 32);
    assert_eq!(size_of::<VkDescriptorPoolCreateInfo>(), 40);
    assert_eq!(size_of::<VkDescriptorSetAllocateInfo>(), 40);

    assert_eq!(size_of::<VkWriteDescriptorSet>(), 64);
    assert_eq!(offset_of!(VkWriteDescriptorSet, dstSet), 16);
    assert_eq!(offset_of!(VkWriteDescriptorSet, descriptorCount), 32);
    assert_eq!(offset_of!(VkWriteDescriptorSet, descriptorType), 36);
    assert_eq!(offset_of!(VkWriteDescriptorSet, pImageInfo), 40);
}

#[test]
fn command_structures_match() {
    assert_eq!(size_of::<VkCommandPoolCreateInfo>(), 24);
    assert_eq!(size_of::<VkCommandBufferAllocateInfo>(), 32);
    assert_eq!(size_of::<VkCommandBufferBeginInfo>(), 32);
    assert_eq!(size_of::<VkFenceCreateInfo>(), 24);
    assert_eq!(size_of::<VkSemaphoreCreateInfo>(), 24);
    assert_eq!(size_of::<VkQueryPoolCreateInfo>(), 32);
    assert_eq!(offset_of!(VkQueryPoolCreateInfo, queryType), 20);
    assert_eq!(offset_of!(VkQueryPoolCreateInfo, queryCount), 24);

    assert_eq!(size_of::<VkSubmitInfo>(), 72);
    assert_eq!(offset_of!(VkSubmitInfo, pWaitSemaphores), 24);
    assert_eq!(offset_of!(VkSubmitInfo, pWaitDstStageMask), 32);
    assert_eq!(offset_of!(VkSubmitInfo, commandBufferCount), 40);
    assert_eq!(offset_of!(VkSubmitInfo, pCommandBuffers), 48);
    assert_eq!(offset_of!(VkSubmitInfo, pSignalSemaphores), 64);
}

#[test]
fn debug_structures_match() {
    assert_eq!(size_of::<VkDebugUtilsMessengerCreateInfoEXT>(), 48);
    assert_eq!(offset_of!(VkDebugUtilsMessengerCreateInfoEXT, messageSeverity), 20);
    assert_eq!(offset_of!(VkDebugUtilsMessengerCreateInfoEXT, pfnUserCallback), 32);

    assert_eq!(size_of::<VkDebugUtilsMessengerCallbackDataEXT>(), 96);
    assert_eq!(offset_of!(VkDebugUtilsMessengerCallbackDataEXT, pMessage), 40);
}

#[test]
fn structure_type_codes_match() {
    // A wrong code is rejected by the loader with a message that names the
    // structure it expected, which is confusing rather than informative.
    assert_eq!(VK_STRUCTURE_TYPE_APPLICATION_INFO, 0);
    assert_eq!(VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, 1);
    assert_eq!(VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, 3);
    assert_eq!(VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, 45);
    assert_eq!(VK_STRUCTURE_TYPE_SWAPCHAIN_CREATE_INFO_KHR, 1_000_001_000);
    assert_eq!(VK_STRUCTURE_TYPE_WIN32_SURFACE_CREATE_INFO_KHR, 1_000_009_000);
}

#[test]
fn zeroed_defaults_stamp_the_structure_type() {
    // The zero pattern is a valid value for every field, but nought is itself a
    // legitimate structure type code, so a create info that forgot to stamp its
    // own type would be read as an application info.
    assert_eq!(
        VkInstanceCreateInfo::default().sType,
        VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO
    );
    assert_eq!(
        VkImageMemoryBarrier::default().sType,
        VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER
    );
    assert_eq!(
        VkGraphicsPipelineCreateInfo::default().sType,
        VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO
    );
    assert!(VkDeviceCreateInfo::default().pEnabledFeatures.is_null());
}