//! Instance creation, layer and extension selection, debug messenger.
//!
//! Owns the loader and the instance level dispatch table. Everything that takes
//! an instance or a physical device is reached through `fns`; the handle is
//! carried beside it because the raw calls need both.

use std::ffi::{c_char, c_void};

use crate::core::{Error, Result};
use crate::render::vk::*;

pub struct Instance {
    /// Kept alive for the whole session: the loader library must not be
    /// unloaded while a driver holds thread local state behind it.
    pub entry: Entry,
    pub handle: VkInstance,
    pub fns: InstanceFns,
    pub api_version: u32,
    debug_messenger: VkDebugUtilsMessengerEXT,
}

impl Instance {
    pub fn new(validation: bool) -> Result<Instance> {
        let entry = Entry::load()?;

        // Version 1.1 is requested when the loader can report one. The renderer
        // needs nothing beyond 1.0, but the validation layer covers 1.1 better
        // and the maintenance revisions correct specification defects that a
        // 1.0 only driver still exhibits.
        let available = match entry.enumerate_instance_version {
            Some(query) => {
                let mut value = make_api_version(0, 1, 0, 0);
                let r = unsafe { query(&mut value) };
                if r < 0 {
                    return Err(vk_err("vkEnumerateInstanceVersion", r));
                }
                value
            }
            None => make_api_version(0, 1, 0, 0),
        };
        let api_version = if available >= make_api_version(0, 1, 1, 0) {
            make_api_version(0, 1, 1, 0)
        } else {
            make_api_version(0, 1, 0, 0)
        };

        let supported = enumerate(|count, data| unsafe {
            (entry.enumerate_instance_extension_properties)(std::ptr::null(), count, data)
        })
        .map_err(|r| vk_err("vkEnumerateInstanceExtensionProperties", r))?;

        let has_extension = |wanted: &[u8]| {
            supported
                .iter()
                .any(|e: &VkExtensionProperties| name_matches(&e.extensionName, wanted))
        };

        for required in [
            VK_KHR_SURFACE_EXTENSION_NAME,
            VK_KHR_WIN32_SURFACE_EXTENSION_NAME,
        ] {
            if !has_extension(required) {
                let name = String::from_utf8_lossy(&required[..required.len() - 1]);
                return Err(Error::vulkan(format!(
                    "instance extension {} is not available",
                    name
                )));
            }
        }

        let mut extension_ptrs: Vec<*const c_char> = vec![
            VK_KHR_SURFACE_EXTENSION_NAME.as_ptr() as *const c_char,
            VK_KHR_WIN32_SURFACE_EXTENSION_NAME.as_ptr() as *const c_char,
        ];

        let debug_available = has_extension(VK_EXT_DEBUG_UTILS_EXTENSION_NAME);
        let use_debug = validation && debug_available;
        if use_debug {
            extension_ptrs.push(VK_EXT_DEBUG_UTILS_EXTENSION_NAME.as_ptr() as *const c_char);
        } else if validation {
            crate::log_warn!(
                "render",
                "VK_EXT_debug_utils is missing, validation messages will not be logged"
            );
        }

        // The layer ships with the software development kit and is absent on an
        // end user machine, so its absence is reported and ignored rather than
        // treated as a configuration error.
        let mut layer_ptrs: Vec<*const c_char> = Vec::new();
        if validation {
            let layers = enumerate(|count, data| unsafe {
                (entry.enumerate_instance_layer_properties)(count, data)
            })
            .map_err(|r| vk_err("vkEnumerateInstanceLayerProperties", r))?;

            let found = layers.iter().any(|l: &VkLayerProperties| {
                name_matches(&l.layerName, VK_LAYER_KHRONOS_VALIDATION_NAME)
            });
            if found {
                layer_ptrs.push(VK_LAYER_KHRONOS_VALIDATION_NAME.as_ptr() as *const c_char);
                crate::log_info!("render", "validation layer enabled");
            } else {
                crate::log_warn!(
                    "render",
                    "validation requested but the layer is not installed"
                );
            }
        }

        let app_info = VkApplicationInfo {
            pApplicationName: b"RXScope\0".as_ptr() as *const c_char,
            applicationVersion: make_api_version(0, 0, 1, 0),
            pEngineName: b"RXScope Renderer\0".as_ptr() as *const c_char,
            engineVersion: make_api_version(0, 0, 1, 0),
            apiVersion: api_version,
            ..Default::default()
        };

        let create_info = VkInstanceCreateInfo {
            pApplicationInfo: &app_info,
            enabledLayerCount: layer_ptrs.len() as u32,
            ppEnabledLayerNames: layer_ptrs.as_ptr(),
            enabledExtensionCount: extension_ptrs.len() as u32,
            ppEnabledExtensionNames: extension_ptrs.as_ptr(),
            ..Default::default()
        };

        let mut handle: VkInstance = std::ptr::null_mut();
        check("vkCreateInstance", unsafe {
            (entry.create_instance)(&create_info, NO_ALLOCATOR, &mut handle)
        })?;
        if handle.is_null() {
            return Err(Error::vulkan("vkCreateInstance returned a null handle"));
        }

        let fns = unsafe { InstanceFns::load(&entry, handle, use_debug)? };

        let mut instance = Instance {
            entry,
            handle,
            fns,
            api_version,
            debug_messenger: VK_NULL_HANDLE,
        };

        if use_debug {
            instance.create_messenger()?;
        }

        crate::log_info!(
            "render",
            "instance created, api {}.{}.{}",
            api_version_major(api_version),
            api_version_minor(api_version),
            api_version_patch(api_version)
        );
        Ok(instance)
    }

    fn create_messenger(&mut self) -> Result<()> {
        let create = match self.fns.create_debug_utils_messenger_ext {
            Some(f) => f,
            None => return Ok(()),
        };

        // Verbose is deliberately excluded. It reports every object creation,
        // which at the rate this renderer creates glyph atlas uploads would
        // dominate the log and hide the messages worth reading.
        let info = VkDebugUtilsMessengerCreateInfoEXT {
            messageSeverity: VK_DEBUG_UTILS_MESSAGE_SEVERITY_ERROR_BIT_EXT
                | VK_DEBUG_UTILS_MESSAGE_SEVERITY_WARNING_BIT_EXT
                | VK_DEBUG_UTILS_MESSAGE_SEVERITY_INFO_BIT_EXT,
            messageType: VK_DEBUG_UTILS_MESSAGE_TYPE_GENERAL_BIT_EXT
                | VK_DEBUG_UTILS_MESSAGE_TYPE_VALIDATION_BIT_EXT
                | VK_DEBUG_UTILS_MESSAGE_TYPE_PERFORMANCE_BIT_EXT,
            pfnUserCallback: Some(debug_callback),
            ..Default::default()
        };

        let mut messenger: VkDebugUtilsMessengerEXT = VK_NULL_HANDLE;
        check("vkCreateDebugUtilsMessengerEXT", unsafe {
            create(self.handle, &info, NO_ALLOCATOR, &mut messenger)
        })?;
        self.debug_messenger = messenger;
        Ok(())
    }

    pub fn destroy(&mut self) {
        unsafe {
            if self.debug_messenger != VK_NULL_HANDLE {
                if let Some(destroy) = self.fns.destroy_debug_utils_messenger_ext {
                    destroy(self.handle, self.debug_messenger, NO_ALLOCATOR);
                }
                self.debug_messenger = VK_NULL_HANDLE;
            }
            if !self.handle.is_null() {
                (self.fns.destroy_instance)(self.handle, NO_ALLOCATOR);
                self.handle = std::ptr::null_mut();
            }
        }
    }
}

/// Validation and driver messages.
///
/// Returning false lets the offending call proceed. Aborting it instead is only
/// useful while hunting one specific defect, and as a permanent setting it turns
/// a warning about a harmless usage into a crash.
unsafe extern "system" fn debug_callback(
    severity: VkDebugUtilsMessageSeverityFlagsEXT,
    kind: VkDebugUtilsMessageTypeFlagsEXT,
    data: *const VkDebugUtilsMessengerCallbackDataEXT,
    _user: *mut c_void,
) -> VkBool32 {
    if data.is_null() {
        return VK_FALSE;
    }

    let message = string_from_ptr((*data).pMessage);
    let message = if message.is_empty() {
        "no message".to_string()
    } else {
        message
    };

    let tag = if kind & VK_DEBUG_UTILS_MESSAGE_TYPE_VALIDATION_BIT_EXT != 0 {
        "validation"
    } else if kind & VK_DEBUG_UTILS_MESSAGE_TYPE_PERFORMANCE_BIT_EXT != 0 {
        "performance"
    } else {
        "general"
    };

    if severity & VK_DEBUG_UTILS_MESSAGE_SEVERITY_ERROR_BIT_EXT != 0 {
        crate::log_error!("vulkan", "{}: {}", tag, message);
    } else if severity & VK_DEBUG_UTILS_MESSAGE_SEVERITY_WARNING_BIT_EXT != 0 {
        crate::log_warn!("vulkan", "{}: {}", tag, message);
    } else {
        crate::log_debug!("vulkan", "{}: {}", tag, message);
    }

    VK_FALSE
}