//! The C ABI of `pyrowave.h` (API 0.6, upstream 89f7e47) and the few Vulkan
//! types it takes, written out by hand: a dozen entry points don't need a
//! binding generator or a Vulkan crate.

#![allow(non_camel_case_types, dead_code)]

use std::ffi::{c_char, c_int, c_void};

// ---- Vulkan ----

pub type VkImage = u64;
pub type VkSemaphore = u64;
pub type VkFormat = c_int;

pub const VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO: c_int = 14;
pub const VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_EXPLICIT_CREATE_INFO_EXT: c_int =
    1_000_158_004;
pub const VK_IMAGE_TYPE_2D: c_int = 1;
pub const VK_FORMAT_R8_UNORM: VkFormat = 9;
pub const VK_FORMAT_R8G8B8A8_UNORM: VkFormat = 37;
pub const VK_FORMAT_B8G8R8A8_UNORM: VkFormat = 44;
pub const VK_SAMPLE_COUNT_1_BIT: u32 = 1;
pub const VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT: c_int = 1_000_158_000;
pub const VK_IMAGE_USAGE_SAMPLED_BIT: u32 = 0x4;
pub const VK_SHARING_MODE_EXCLUSIVE: c_int = 0;
pub const VK_IMAGE_LAYOUT_UNDEFINED: c_int = 0;
pub const VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT: c_int = 0x200;
pub const VK_IMAGE_ASPECT_COLOR_BIT: c_int = 0x1;
/// `VK_QUEUE_FAMILY_FOREIGN_EXT`: owned by another API or device (our GL compositor).
pub const VK_QUEUE_FAMILY_FOREIGN_EXT: u32 = !2;
pub const VK_COLOR_SPACE_SRGB_NONLINEAR_KHR: c_int = 0;
pub const VK_QUEUE_GLOBAL_PRIORITY_MEDIUM: c_int = 256;

#[repr(C)]
pub struct VkExtent3D {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
}

#[repr(C)]
pub struct VkImageCreateInfo {
    pub s_type: c_int,
    pub p_next: *const c_void,
    pub flags: u32,
    pub image_type: c_int,
    pub format: VkFormat,
    pub extent: VkExtent3D,
    pub mip_levels: u32,
    pub array_layers: u32,
    pub samples: u32,
    pub tiling: c_int,
    pub usage: u32,
    pub sharing_mode: c_int,
    pub queue_family_index_count: u32,
    pub p_queue_family_indices: *const u32,
    pub initial_layout: c_int,
}

#[repr(C)]
pub struct VkSubresourceLayout {
    pub offset: u64,
    pub size: u64,
    pub row_pitch: u64,
    pub array_pitch: u64,
    pub depth_pitch: u64,
}

#[repr(C)]
pub struct VkImageDrmFormatModifierExplicitCreateInfoEXT {
    pub s_type: c_int,
    pub p_next: *const c_void,
    pub drm_format_modifier: u64,
    pub drm_format_modifier_plane_count: u32,
    pub p_plane_layouts: *const VkSubresourceLayout,
}

#[repr(C)]
pub struct VkRect2D {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

// ---- PyroWave ----

pub type pyrowave_result = c_int;
pub const PYROWAVE_SUCCESS: pyrowave_result = 0;

pub type pyrowave_device = *mut c_void;
pub type pyrowave_encoder = *mut c_void;
pub type pyrowave_image = *mut c_void;

pub const PYROWAVE_CHROMA_SUBSAMPLING_420: c_int = 0;
pub const PYROWAVE_CHROMA_SUBSAMPLING_444: c_int = 1;

#[repr(C)]
pub struct pyrowave_uuid {
    pub uuid: [u8; 16],
}

#[repr(C)]
pub struct pyrowave_luid {
    pub luid: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct pyrowave_image_view {
    pub image: VkImage,
    pub width: u32,
    pub height: u32,
    pub image_format: VkFormat,
    pub view_format: VkFormat,
    pub mip_level: u32,
    pub layer: u32,
    pub aspect: c_int,
    pub swizzle: c_int,
    pub layout: c_int,
}

#[repr(C)]
pub struct pyrowave_image_create_info {
    pub device: pyrowave_device,
    pub external_handle: usize,
    pub handle_type: c_int,
    pub image_create_info: *const VkImageCreateInfo,
}

#[repr(C)]
pub struct pyrowave_encoder_create_info {
    pub device: pyrowave_device,
    pub width: c_int,
    pub height: c_int,
    pub chroma: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct pyrowave_packet {
    pub offset: usize,
    pub size: usize,
}

#[repr(C)]
pub struct pyrowave_sync_point {
    pub semaphore: VkSemaphore,
    pub value: u64,
}

#[repr(C)]
pub struct pyrowave_gpu_external_reference {
    pub image: pyrowave_image,
    pub queue_family_index: u32,
}

#[repr(C)]
pub struct pyrowave_gpu_sync_operation {
    pub images: *const pyrowave_gpu_external_reference,
    pub num_images: usize,
    pub sync: pyrowave_sync_point,
}

#[repr(C)]
pub struct pyrowave_scaled_encode_info {
    pub view: pyrowave_image_view,
    pub input_color_space: c_int,
    pub output_color_space: c_int,
    pub intermediate_plane_format: VkFormat,
    pub ycbcr_chroma_midpoint: f32,
    pub force_linear_filtering: bool,
    pub skip_dither: bool,
    pub crop_rect: *const VkRect2D,
}

#[repr(C)]
pub struct pyrowave_rate_control {
    pub maximum_bitstream_size: usize,
}

pub type pyrowave_message_cb = unsafe extern "C" fn(userdata: *mut c_void, msg: *const c_char);

/// The entry points we use, resolved from the library.
pub struct Api {
    pub get_api_version: unsafe extern "C" fn(*mut u32, *mut u32, *mut u32),
    pub create_device_by_compat2: unsafe extern "C" fn(
        u32,
        u32,
        *const pyrowave_uuid,
        *const pyrowave_uuid,
        *const pyrowave_luid,
        c_int,
        *mut pyrowave_device,
    ) -> pyrowave_result,
    pub device_destroy: unsafe extern "C" fn(pyrowave_device),
    pub device_report_performance_stats:
        unsafe extern "C" fn(pyrowave_device, pyrowave_message_cb, *mut c_void, bool),
    pub image_create: unsafe extern "C" fn(
        *const pyrowave_image_create_info,
        *mut pyrowave_image,
    ) -> pyrowave_result,
    pub image_get_image_view: unsafe extern "C" fn(
        pyrowave_image,
        c_int,
        c_int,
        *mut pyrowave_image_view,
    ) -> pyrowave_result,
    pub image_destroy: unsafe extern "C" fn(pyrowave_image),
    pub encoder_create: unsafe extern "C" fn(
        *const pyrowave_encoder_create_info,
        *mut pyrowave_encoder,
    ) -> pyrowave_result,
    pub encoder_encode_gpu_scaled_synchronous: unsafe extern "C" fn(
        pyrowave_encoder,
        *const pyrowave_gpu_sync_operation,
        *const pyrowave_gpu_sync_operation,
        *const pyrowave_scaled_encode_info,
        *const pyrowave_rate_control,
    ) -> pyrowave_result,
    pub encoder_compute_num_packets:
        unsafe extern "C" fn(pyrowave_encoder, usize, *mut usize) -> pyrowave_result,
    pub encoder_packetize: unsafe extern "C" fn(
        pyrowave_encoder,
        *mut pyrowave_packet,
        usize,
        *mut usize,
        *mut c_void,
        usize,
    ) -> pyrowave_result,
    pub encoder_destroy: unsafe extern "C" fn(pyrowave_encoder),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_match_the_c_headers() {
        // 64-bit Linux, as compiled by GCC/Clang from pyrowave.h and vulkan_core.h.
        assert_eq!(size_of::<pyrowave_image_view>(), 48);
        assert_eq!(size_of::<pyrowave_scaled_encode_info>(), 80);
        assert_eq!(size_of::<pyrowave_gpu_sync_operation>(), 32);
        assert_eq!(size_of::<pyrowave_gpu_external_reference>(), 16);
        assert_eq!(size_of::<pyrowave_image_create_info>(), 32);
        assert_eq!(size_of::<pyrowave_encoder_create_info>(), 24);
        assert_eq!(size_of::<VkImageCreateInfo>(), 88);
        assert_eq!(size_of::<VkSubresourceLayout>(), 40);
        assert_eq!(
            size_of::<VkImageDrmFormatModifierExplicitCreateInfoEXT>(),
            40
        );
    }
}
