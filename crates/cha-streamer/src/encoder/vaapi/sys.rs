//! libva's structs and constants for what the VA-API encoder uses, as bindgen
//! made them from the real headers. Generated, not edited by hand.
//!
//! - Tool: rust-bindgen 0.71.1.
//! - Headers: libva 2.22.0 (Debian 13), MIT-licensed: `va.h`, `va_vpp.h`,
//!   `va_drmcommon.h`, `va_enc_h264.h`, `va_drm.h`. Run on the Intel test
//!   node (x86_64 Linux).
//! - Allowlisted: the H.264 encode parameter buffers, `VAPictureH264`, the
//!   misc parameter buffers (generic, rate control, HRD, frame rate),
//!   `VAProcPipelineParameterBuffer`, `VACodedBufferSegment`,
//!   `VAConfigAttrib`, `VASurfaceAttrib`, `VAGenericValue`, `VARectangle`,
//!   `VADRMPRIMESurfaceDescriptor`, and the `VA_*` macros. Regenerate with
//!   a header that includes those five, then:
//!
//!   ```text
//!   bindgen va-wrap.h --no-doc-comments --use-core --ctypes-prefix core::ffi \
//!     --allowlist-type 'VA(Picture|EncSequenceParameterBuffer|EncPictureParameterBuffer|EncSliceParameterBuffer)H264' \
//!     --allowlist-type 'VAEnc(PackedHeaderParameterBuffer|MiscParameterBuffer|MiscParameterRateControl|MiscParameterHRD|MiscParameterFrameRate)' \
//!     --allowlist-type 'VA(ProcPipelineParameterBuffer|CodedBufferSegment|ConfigAttrib|SurfaceAttrib|GenericValue|Rectangle|DRMPRIMESurfaceDescriptor)' \
//!     --allowlist-var 'VA_.*' -o sys.rs
//!   ```
//! - The layout checks are the x86_64 Linux sizes and offsets; they hold on
//!   other LP64 targets (aarch64 macOS included).

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct __BindgenBitfieldUnit<Storage> {
    storage: Storage,
}
impl<Storage> __BindgenBitfieldUnit<Storage> {
    #[inline]
    pub const fn new(storage: Storage) -> Self {
        Self { storage }
    }
}
impl<Storage> __BindgenBitfieldUnit<Storage>
where
    Storage: AsRef<[u8]> + AsMut<[u8]>,
{
    #[inline]
    fn extract_bit(byte: u8, index: usize) -> bool {
        let bit_index = if cfg!(target_endian = "big") {
            7 - (index % 8)
        } else {
            index % 8
        };
        let mask = 1 << bit_index;
        byte & mask == mask
    }
    #[inline]
    pub fn get_bit(&self, index: usize) -> bool {
        debug_assert!(index / 8 < self.storage.as_ref().len());
        let byte_index = index / 8;
        let byte = self.storage.as_ref()[byte_index];
        Self::extract_bit(byte, index)
    }
    #[inline]
    pub unsafe fn raw_get_bit(this: *const Self, index: usize) -> bool {
        debug_assert!(index / 8 < core::mem::size_of::<Storage>());
        let byte_index = index / 8;
        let byte = unsafe {
            *(core::ptr::addr_of!((*this).storage) as *const u8).offset(byte_index as isize)
        };
        Self::extract_bit(byte, index)
    }
    #[inline]
    fn change_bit(byte: u8, index: usize, val: bool) -> u8 {
        let bit_index = if cfg!(target_endian = "big") {
            7 - (index % 8)
        } else {
            index % 8
        };
        let mask = 1 << bit_index;
        if val { byte | mask } else { byte & !mask }
    }
    #[inline]
    pub fn set_bit(&mut self, index: usize, val: bool) {
        debug_assert!(index / 8 < self.storage.as_ref().len());
        let byte_index = index / 8;
        let byte = &mut self.storage.as_mut()[byte_index];
        *byte = Self::change_bit(*byte, index, val);
    }
    #[inline]
    pub unsafe fn raw_set_bit(this: *mut Self, index: usize, val: bool) {
        debug_assert!(index / 8 < core::mem::size_of::<Storage>());
        let byte_index = index / 8;
        let byte = unsafe {
            (core::ptr::addr_of_mut!((*this).storage) as *mut u8).offset(byte_index as isize)
        };
        unsafe { *byte = Self::change_bit(*byte, index, val) };
    }
    #[inline]
    pub fn get(&self, bit_offset: usize, bit_width: u8) -> u64 {
        debug_assert!(bit_width <= 64);
        debug_assert!(bit_offset / 8 < self.storage.as_ref().len());
        debug_assert!((bit_offset + (bit_width as usize)) / 8 <= self.storage.as_ref().len());
        let mut val = 0;
        for i in 0..(bit_width as usize) {
            if self.get_bit(i + bit_offset) {
                let index = if cfg!(target_endian = "big") {
                    bit_width as usize - 1 - i
                } else {
                    i
                };
                val |= 1 << index;
            }
        }
        val
    }
    #[inline]
    pub unsafe fn raw_get(this: *const Self, bit_offset: usize, bit_width: u8) -> u64 {
        debug_assert!(bit_width <= 64);
        debug_assert!(bit_offset / 8 < core::mem::size_of::<Storage>());
        debug_assert!((bit_offset + (bit_width as usize)) / 8 <= core::mem::size_of::<Storage>());
        let mut val = 0;
        for i in 0..(bit_width as usize) {
            if unsafe { Self::raw_get_bit(this, i + bit_offset) } {
                let index = if cfg!(target_endian = "big") {
                    bit_width as usize - 1 - i
                } else {
                    i
                };
                val |= 1 << index;
            }
        }
        val
    }
    #[inline]
    pub fn set(&mut self, bit_offset: usize, bit_width: u8, val: u64) {
        debug_assert!(bit_width <= 64);
        debug_assert!(bit_offset / 8 < self.storage.as_ref().len());
        debug_assert!((bit_offset + (bit_width as usize)) / 8 <= self.storage.as_ref().len());
        for i in 0..(bit_width as usize) {
            let mask = 1 << i;
            let val_bit_is_set = val & mask == mask;
            let index = if cfg!(target_endian = "big") {
                bit_width as usize - 1 - i
            } else {
                i
            };
            self.set_bit(index + bit_offset, val_bit_is_set);
        }
    }
    #[inline]
    pub unsafe fn raw_set(this: *mut Self, bit_offset: usize, bit_width: u8, val: u64) {
        debug_assert!(bit_width <= 64);
        debug_assert!(bit_offset / 8 < core::mem::size_of::<Storage>());
        debug_assert!((bit_offset + (bit_width as usize)) / 8 <= core::mem::size_of::<Storage>());
        for i in 0..(bit_width as usize) {
            let mask = 1 << i;
            let val_bit_is_set = val & mask == mask;
            let index = if cfg!(target_endian = "big") {
                bit_width as usize - 1 - i
            } else {
                i
            };
            unsafe { Self::raw_set_bit(this, index + bit_offset, val_bit_is_set) };
        }
    }
}
#[repr(C)]
#[derive(Default)]
pub struct __IncompleteArrayField<T>(::core::marker::PhantomData<T>, [T; 0]);
impl<T> __IncompleteArrayField<T> {
    #[inline]
    pub const fn new() -> Self {
        __IncompleteArrayField(::core::marker::PhantomData, [])
    }
    #[inline]
    pub fn as_ptr(&self) -> *const T {
        self as *const _ as *const T
    }
    #[inline]
    pub fn as_mut_ptr(&mut self) -> *mut T {
        self as *mut _ as *mut T
    }
    #[inline]
    pub unsafe fn as_slice(&self, len: usize) -> &[T] {
        ::core::slice::from_raw_parts(self.as_ptr(), len)
    }
    #[inline]
    pub unsafe fn as_mut_slice(&mut self, len: usize) -> &mut [T] {
        ::core::slice::from_raw_parts_mut(self.as_mut_ptr(), len)
    }
}
impl<T> ::core::fmt::Debug for __IncompleteArrayField<T> {
    fn fmt(&self, fmt: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        fmt.write_str("__IncompleteArrayField")
    }
}
pub const VA_MAJOR_VERSION: u32 = 1;
pub const VA_MINOR_VERSION: u32 = 22;
pub const VA_MICRO_VERSION: u32 = 0;
pub const VA_VERSION_S: &[u8; 7] = b"1.22.0\0";
pub const VA_VERSION_HEX: u32 = 18219008;
pub const VA_STATUS_SUCCESS: u32 = 0;
pub const VA_STATUS_ERROR_OPERATION_FAILED: u32 = 1;
pub const VA_STATUS_ERROR_ALLOCATION_FAILED: u32 = 2;
pub const VA_STATUS_ERROR_INVALID_DISPLAY: u32 = 3;
pub const VA_STATUS_ERROR_INVALID_CONFIG: u32 = 4;
pub const VA_STATUS_ERROR_INVALID_CONTEXT: u32 = 5;
pub const VA_STATUS_ERROR_INVALID_SURFACE: u32 = 6;
pub const VA_STATUS_ERROR_INVALID_BUFFER: u32 = 7;
pub const VA_STATUS_ERROR_INVALID_IMAGE: u32 = 8;
pub const VA_STATUS_ERROR_INVALID_SUBPICTURE: u32 = 9;
pub const VA_STATUS_ERROR_ATTR_NOT_SUPPORTED: u32 = 10;
pub const VA_STATUS_ERROR_MAX_NUM_EXCEEDED: u32 = 11;
pub const VA_STATUS_ERROR_UNSUPPORTED_PROFILE: u32 = 12;
pub const VA_STATUS_ERROR_UNSUPPORTED_ENTRYPOINT: u32 = 13;
pub const VA_STATUS_ERROR_UNSUPPORTED_RT_FORMAT: u32 = 14;
pub const VA_STATUS_ERROR_UNSUPPORTED_BUFFERTYPE: u32 = 15;
pub const VA_STATUS_ERROR_SURFACE_BUSY: u32 = 16;
pub const VA_STATUS_ERROR_FLAG_NOT_SUPPORTED: u32 = 17;
pub const VA_STATUS_ERROR_INVALID_PARAMETER: u32 = 18;
pub const VA_STATUS_ERROR_RESOLUTION_NOT_SUPPORTED: u32 = 19;
pub const VA_STATUS_ERROR_UNIMPLEMENTED: u32 = 20;
pub const VA_STATUS_ERROR_SURFACE_IN_DISPLAYING: u32 = 21;
pub const VA_STATUS_ERROR_INVALID_IMAGE_FORMAT: u32 = 22;
pub const VA_STATUS_ERROR_DECODING_ERROR: u32 = 23;
pub const VA_STATUS_ERROR_ENCODING_ERROR: u32 = 24;
pub const VA_STATUS_ERROR_INVALID_VALUE: u32 = 25;
pub const VA_STATUS_ERROR_UNSUPPORTED_FILTER: u32 = 32;
pub const VA_STATUS_ERROR_INVALID_FILTER_CHAIN: u32 = 33;
pub const VA_STATUS_ERROR_HW_BUSY: u32 = 34;
pub const VA_STATUS_ERROR_UNSUPPORTED_MEMORY_TYPE: u32 = 36;
pub const VA_STATUS_ERROR_NOT_ENOUGH_BUFFER: u32 = 37;
pub const VA_STATUS_ERROR_TIMEDOUT: u32 = 38;
pub const VA_STATUS_ERROR_UNKNOWN: u32 = 4294967295;
pub const VA_FRAME_PICTURE: u32 = 0;
pub const VA_TOP_FIELD: u32 = 1;
pub const VA_BOTTOM_FIELD: u32 = 2;
pub const VA_TOP_FIELD_FIRST: u32 = 4;
pub const VA_BOTTOM_FIELD_FIRST: u32 = 8;
pub const VA_ENABLE_BLEND: u32 = 4;
pub const VA_CLEAR_DRAWABLE: u32 = 8;
pub const VA_SRC_COLOR_MASK: u32 = 240;
pub const VA_SRC_BT601: u32 = 16;
pub const VA_SRC_BT709: u32 = 32;
pub const VA_SRC_SMPTE_240: u32 = 64;
pub const VA_FILTER_SCALING_DEFAULT: u32 = 0;
pub const VA_FILTER_SCALING_FAST: u32 = 256;
pub const VA_FILTER_SCALING_HQ: u32 = 512;
pub const VA_FILTER_SCALING_NL_ANAMORPHIC: u32 = 768;
pub const VA_FILTER_SCALING_MASK: u32 = 3840;
pub const VA_FILTER_INTERPOLATION_DEFAULT: u32 = 0;
pub const VA_FILTER_INTERPOLATION_NEAREST_NEIGHBOR: u32 = 4096;
pub const VA_FILTER_INTERPOLATION_BILINEAR: u32 = 8192;
pub const VA_FILTER_INTERPOLATION_ADVANCED: u32 = 12288;
pub const VA_FILTER_INTERPOLATION_MASK: u32 = 61440;
pub const VA_PADDING_LOW: u32 = 4;
pub const VA_PADDING_MEDIUM: u32 = 8;
pub const VA_PADDING_HIGH: u32 = 16;
pub const VA_PADDING_LARGE: u32 = 32;
pub const VA_EXEC_SYNC: u32 = 0;
pub const VA_EXEC_ASYNC: u32 = 1;
pub const VA_EXEC_MODE_DEFAULT: u32 = 0;
pub const VA_EXEC_MODE_POWER_SAVING: u32 = 1;
pub const VA_EXEC_MODE_PERFORMANCE: u32 = 2;
pub const VA_FEATURE_NOT_SUPPORTED: u32 = 0;
pub const VA_FEATURE_SUPPORTED: u32 = 1;
pub const VA_FEATURE_REQUIRED: u32 = 2;
pub const VA_RT_FORMAT_YUV420: u32 = 1;
pub const VA_RT_FORMAT_YUV422: u32 = 2;
pub const VA_RT_FORMAT_YUV444: u32 = 4;
pub const VA_RT_FORMAT_YUV411: u32 = 8;
pub const VA_RT_FORMAT_YUV400: u32 = 16;
pub const VA_RT_FORMAT_YUV420_10: u32 = 256;
pub const VA_RT_FORMAT_YUV422_10: u32 = 512;
pub const VA_RT_FORMAT_YUV444_10: u32 = 1024;
pub const VA_RT_FORMAT_YUV420_12: u32 = 4096;
pub const VA_RT_FORMAT_YUV422_12: u32 = 8192;
pub const VA_RT_FORMAT_YUV444_12: u32 = 16384;
pub const VA_RT_FORMAT_RGB16: u32 = 65536;
pub const VA_RT_FORMAT_RGB32: u32 = 131072;
pub const VA_RT_FORMAT_RGBP: u32 = 1048576;
pub const VA_RT_FORMAT_RGB32_10: u32 = 2097152;
pub const VA_RT_FORMAT_PROTECTED: u32 = 2147483648;
pub const VA_RT_FORMAT_RGB32_10BPP: u32 = 2097152;
pub const VA_RT_FORMAT_YUV420_10BPP: u32 = 256;
pub const VA_RC_NONE: u32 = 1;
pub const VA_RC_CBR: u32 = 2;
pub const VA_RC_VBR: u32 = 4;
pub const VA_RC_VCM: u32 = 8;
pub const VA_RC_CQP: u32 = 16;
pub const VA_RC_VBR_CONSTRAINED: u32 = 32;
pub const VA_RC_ICQ: u32 = 64;
pub const VA_RC_MB: u32 = 128;
pub const VA_RC_CFS: u32 = 256;
pub const VA_RC_PARALLEL: u32 = 512;
pub const VA_RC_QVBR: u32 = 1024;
pub const VA_RC_AVBR: u32 = 2048;
pub const VA_RC_TCBRC: u32 = 4096;
pub const VA_DEC_SLICE_MODE_NORMAL: u32 = 1;
pub const VA_DEC_SLICE_MODE_BASE: u32 = 2;
pub const VA_DEC_PROCESSING_NONE: u32 = 0;
pub const VA_DEC_PROCESSING: u32 = 1;
pub const VA_ENC_PACKED_HEADER_NONE: u32 = 0;
pub const VA_ENC_PACKED_HEADER_SEQUENCE: u32 = 1;
pub const VA_ENC_PACKED_HEADER_PICTURE: u32 = 2;
pub const VA_ENC_PACKED_HEADER_SLICE: u32 = 4;
pub const VA_ENC_PACKED_HEADER_MISC: u32 = 8;
pub const VA_ENC_PACKED_HEADER_RAW_DATA: u32 = 16;
pub const VA_ENC_INTERLACED_NONE: u32 = 0;
pub const VA_ENC_INTERLACED_FRAME: u32 = 1;
pub const VA_ENC_INTERLACED_FIELD: u32 = 2;
pub const VA_ENC_INTERLACED_MBAFF: u32 = 4;
pub const VA_ENC_INTERLACED_PAFF: u32 = 8;
pub const VA_ENC_SLICE_STRUCTURE_POWER_OF_TWO_ROWS: u32 = 1;
pub const VA_ENC_SLICE_STRUCTURE_ARBITRARY_MACROBLOCKS: u32 = 2;
pub const VA_ENC_SLICE_STRUCTURE_EQUAL_ROWS: u32 = 4;
pub const VA_ENC_SLICE_STRUCTURE_MAX_SLICE_SIZE: u32 = 8;
pub const VA_ENC_SLICE_STRUCTURE_ARBITRARY_ROWS: u32 = 16;
pub const VA_ENC_SLICE_STRUCTURE_EQUAL_MULTI_ROWS: u32 = 32;
pub const VA_ENC_QUANTIZATION_NONE: u32 = 0;
pub const VA_ENC_QUANTIZATION_TRELLIS_SUPPORTED: u32 = 1;
pub const VA_PREDICTION_DIRECTION_PREVIOUS: u32 = 1;
pub const VA_PREDICTION_DIRECTION_FUTURE: u32 = 2;
pub const VA_PREDICTION_DIRECTION_BI_NOT_EMPTY: u32 = 4;
pub const VA_ENC_INTRA_REFRESH_NONE: u32 = 0;
pub const VA_ENC_INTRA_REFRESH_ROLLING_COLUMN: u32 = 1;
pub const VA_ENC_INTRA_REFRESH_ROLLING_ROW: u32 = 2;
pub const VA_ENC_INTRA_REFRESH_ADAPTIVE: u32 = 16;
pub const VA_ENC_INTRA_REFRESH_CYCLIC: u32 = 32;
pub const VA_ENC_INTRA_REFRESH_P_FRAME: u32 = 65536;
pub const VA_ENC_INTRA_REFRESH_B_FRAME: u32 = 131072;
pub const VA_ENC_INTRA_REFRESH_MULTI_REF: u32 = 262144;
pub const VA_PC_CIPHER_AES: u32 = 1;
pub const VA_PC_BLOCK_SIZE_128: u32 = 1;
pub const VA_PC_BLOCK_SIZE_192: u32 = 2;
pub const VA_PC_BLOCK_SIZE_256: u32 = 4;
pub const VA_PC_CIPHER_MODE_ECB: u32 = 1;
pub const VA_PC_CIPHER_MODE_CBC: u32 = 2;
pub const VA_PC_CIPHER_MODE_CTR: u32 = 4;
pub const VA_PC_SAMPLE_TYPE_FULLSAMPLE: u32 = 1;
pub const VA_PC_SAMPLE_TYPE_SUBSAMPLE: u32 = 2;
pub const VA_PC_USAGE_DEFAULT: u32 = 0;
pub const VA_PC_USAGE_WIDEVINE: u32 = 1;
pub const VA_PROCESSING_RATE_NONE: u32 = 0;
pub const VA_PROCESSING_RATE_ENCODE: u32 = 1;
pub const VA_PROCESSING_RATE_DECODE: u32 = 2;
pub const VA_ATTRIB_NOT_SUPPORTED: u32 = 2147483648;
pub const VA_INVALID_ID: u32 = 4294967295;
pub const VA_INVALID_SURFACE: u32 = 4294967295;
pub const VA_SURFACE_ATTRIB_NOT_SUPPORTED: u32 = 0;
pub const VA_SURFACE_ATTRIB_GETTABLE: u32 = 1;
pub const VA_SURFACE_ATTRIB_SETTABLE: u32 = 2;
pub const VA_SURFACE_ATTRIB_MEM_TYPE_VA: u32 = 1;
pub const VA_SURFACE_ATTRIB_MEM_TYPE_V4L2: u32 = 2;
pub const VA_SURFACE_ATTRIB_MEM_TYPE_USER_PTR: u32 = 4;
pub const VA_SURFACE_EXTBUF_DESC_ENABLE_TILING: u32 = 1;
pub const VA_SURFACE_EXTBUF_DESC_CACHED: u32 = 2;
pub const VA_SURFACE_EXTBUF_DESC_UNCACHED: u32 = 4;
pub const VA_SURFACE_EXTBUF_DESC_WC: u32 = 8;
pub const VA_SURFACE_EXTBUF_DESC_PROTECTED: u32 = 2147483648;
pub const VA_SURFACE_ATTRIB_USAGE_HINT_GENERIC: u32 = 0;
pub const VA_SURFACE_ATTRIB_USAGE_HINT_DECODER: u32 = 1;
pub const VA_SURFACE_ATTRIB_USAGE_HINT_ENCODER: u32 = 2;
pub const VA_SURFACE_ATTRIB_USAGE_HINT_VPP_READ: u32 = 4;
pub const VA_SURFACE_ATTRIB_USAGE_HINT_VPP_WRITE: u32 = 8;
pub const VA_SURFACE_ATTRIB_USAGE_HINT_DISPLAY: u32 = 16;
pub const VA_SURFACE_ATTRIB_USAGE_HINT_EXPORT: u32 = 32;
pub const VA_PROGRESSIVE: u32 = 1;
pub const VA_ENCRYPTION_TYPE_FULLSAMPLE_CTR: u32 = 1;
pub const VA_ENCRYPTION_TYPE_FULLSAMPLE_CBC: u32 = 2;
pub const VA_ENCRYPTION_TYPE_SUBSAMPLE_CTR: u32 = 4;
pub const VA_ENCRYPTION_TYPE_SUBSAMPLE_CBC: u32 = 8;
pub const VA_SLICE_DATA_FLAG_ALL: u32 = 0;
pub const VA_SLICE_DATA_FLAG_BEGIN: u32 = 1;
pub const VA_SLICE_DATA_FLAG_MIDDLE: u32 = 2;
pub const VA_SLICE_DATA_FLAG_END: u32 = 4;
pub const VA_MB_TYPE_MOTION_FORWARD: u32 = 2;
pub const VA_MB_TYPE_MOTION_BACKWARD: u32 = 4;
pub const VA_MB_TYPE_MOTION_PATTERN: u32 = 8;
pub const VA_MB_TYPE_MOTION_INTRA: u32 = 16;
pub const VA_PICTURE_H264_INVALID: u32 = 1;
pub const VA_PICTURE_H264_TOP_FIELD: u32 = 2;
pub const VA_PICTURE_H264_BOTTOM_FIELD: u32 = 4;
pub const VA_PICTURE_H264_SHORT_TERM_REFERENCE: u32 = 8;
pub const VA_PICTURE_H264_LONG_TERM_REFERENCE: u32 = 16;
pub const VA_CODED_BUF_STATUS_PICTURE_AVE_QP_MASK: u32 = 255;
pub const VA_CODED_BUF_STATUS_LARGE_SLICE_MASK: u32 = 256;
pub const VA_CODED_BUF_STATUS_SLICE_OVERFLOW_MASK: u32 = 512;
pub const VA_CODED_BUF_STATUS_BITRATE_OVERFLOW: u32 = 1024;
pub const VA_CODED_BUF_STATUS_BITRATE_HIGH: u32 = 2048;
pub const VA_CODED_BUF_STATUS_FRAME_SIZE_OVERFLOW: u32 = 4096;
pub const VA_CODED_BUF_STATUS_BAD_BITSTREAM: u32 = 32768;
pub const VA_CODED_BUF_STATUS_AIR_MB_OVER_THRESHOLD: u32 = 16711680;
pub const VA_CODED_BUF_STATUS_NUMBER_PASSES_MASK: u32 = 251658240;
pub const VA_CODED_BUF_STATUS_SINGLE_NALU: u32 = 268435456;
pub const VA_MAPBUFFER_FLAG_DEFAULT: u32 = 0;
pub const VA_MAPBUFFER_FLAG_READ: u32 = 1;
pub const VA_MAPBUFFER_FLAG_WRITE: u32 = 2;
pub const VA_EXPORT_SURFACE_READ_ONLY: u32 = 1;
pub const VA_EXPORT_SURFACE_WRITE_ONLY: u32 = 2;
pub const VA_EXPORT_SURFACE_READ_WRITE: u32 = 3;
pub const VA_EXPORT_SURFACE_SEPARATE_LAYERS: u32 = 4;
pub const VA_EXPORT_SURFACE_COMPOSED_LAYERS: u32 = 8;
pub const VA_TIMEOUT_INFINITE: i32 = -1;
pub const VA_FOURCC_NV12: u32 = 842094158;
pub const VA_FOURCC_NV21: u32 = 825382478;
pub const VA_FOURCC_AI44: u32 = 875839817;
pub const VA_FOURCC_RGBA: u32 = 1094862674;
pub const VA_FOURCC_RGBX: u32 = 1480738642;
pub const VA_FOURCC_BGRA: u32 = 1095911234;
pub const VA_FOURCC_BGRX: u32 = 1481787202;
pub const VA_FOURCC_ARGB: u32 = 1111970369;
pub const VA_FOURCC_XRGB: u32 = 1111970392;
pub const VA_FOURCC_ABGR: u32 = 1380401729;
pub const VA_FOURCC_XBGR: u32 = 1380401752;
pub const VA_FOURCC_UYVY: u32 = 1498831189;
pub const VA_FOURCC_YUY2: u32 = 844715353;
pub const VA_FOURCC_AYUV: u32 = 1448433985;
pub const VA_FOURCC_NV11: u32 = 825316942;
pub const VA_FOURCC_YV12: u32 = 842094169;
pub const VA_FOURCC_P208: u32 = 942682704;
pub const VA_FOURCC_I420: u32 = 808596553;
pub const VA_FOURCC_YV24: u32 = 875714137;
pub const VA_FOURCC_YV32: u32 = 842225241;
pub const VA_FOURCC_Y800: u32 = 808466521;
pub const VA_FOURCC_IMC3: u32 = 860048713;
pub const VA_FOURCC_411P: u32 = 1345401140;
pub const VA_FOURCC_411R: u32 = 1378955572;
pub const VA_FOURCC_422H: u32 = 1211249204;
pub const VA_FOURCC_422V: u32 = 1446130228;
pub const VA_FOURCC_444P: u32 = 1345598516;
pub const VA_FOURCC_RGBP: u32 = 1346520914;
pub const VA_FOURCC_BGRP: u32 = 1347569474;
pub const VA_FOURCC_RGB565: u32 = 909199186;
pub const VA_FOURCC_BGR565: u32 = 909199170;
pub const VA_FOURCC_Y210: u32 = 808530521;
pub const VA_FOURCC_Y212: u32 = 842084953;
pub const VA_FOURCC_Y216: u32 = 909193817;
pub const VA_FOURCC_Y410: u32 = 808531033;
pub const VA_FOURCC_Y412: u32 = 842085465;
pub const VA_FOURCC_Y416: u32 = 909194329;
pub const VA_FOURCC_YV16: u32 = 909203033;
pub const VA_FOURCC_P010: u32 = 808530000;
pub const VA_FOURCC_P012: u32 = 842084432;
pub const VA_FOURCC_P016: u32 = 909193296;
pub const VA_FOURCC_I010: u32 = 808529993;
pub const VA_FOURCC_IYUV: u32 = 1448433993;
pub const VA_FOURCC_A2R10G10B10: u32 = 808669761;
pub const VA_FOURCC_A2B10G10R10: u32 = 808665665;
pub const VA_FOURCC_X2R10G10B10: u32 = 808669784;
pub const VA_FOURCC_X2B10G10R10: u32 = 808665688;
pub const VA_FOURCC_Y8: u32 = 538982489;
pub const VA_FOURCC_Y16: u32 = 540422489;
pub const VA_FOURCC_VYUY: u32 = 1498765654;
pub const VA_FOURCC_YVYU: u32 = 1431918169;
pub const VA_FOURCC_ARGB64: u32 = 877089345;
pub const VA_FOURCC_ABGR64: u32 = 877085249;
pub const VA_FOURCC_XYUV: u32 = 1448434008;
pub const VA_FOURCC_Q416: u32 = 909194321;
pub const VA_LSB_FIRST: u32 = 1;
pub const VA_MSB_FIRST: u32 = 2;
pub const VA_SUBPICTURE_CHROMA_KEYING: u32 = 1;
pub const VA_SUBPICTURE_GLOBAL_ALPHA: u32 = 2;
pub const VA_SUBPICTURE_DESTINATION_IS_SCREEN_COORD: u32 = 4;
pub const VA_ROTATION_NONE: u32 = 0;
pub const VA_ROTATION_90: u32 = 1;
pub const VA_ROTATION_180: u32 = 2;
pub const VA_ROTATION_270: u32 = 3;
pub const VA_MIRROR_NONE: u32 = 0;
pub const VA_MIRROR_HORIZONTAL: u32 = 1;
pub const VA_MIRROR_VERTICAL: u32 = 2;
pub const VA_OOL_DEBLOCKING_FALSE: u32 = 0;
pub const VA_OOL_DEBLOCKING_TRUE: u32 = 1;
pub const VA_RENDER_MODE_UNDEFINED: u32 = 0;
pub const VA_RENDER_MODE_LOCAL_OVERLAY: u32 = 1;
pub const VA_RENDER_MODE_LOCAL_GPU: u32 = 2;
pub const VA_RENDER_MODE_EXTERNAL_OVERLAY: u32 = 4;
pub const VA_RENDER_MODE_EXTERNAL_GPU: u32 = 8;
pub const VA_RENDER_DEVICE_UNDEFINED: u32 = 0;
pub const VA_RENDER_DEVICE_LOCAL: u32 = 1;
pub const VA_RENDER_DEVICE_EXTERNAL: u32 = 2;
pub const VA_DISPLAY_ATTRIB_NOT_SUPPORTED: u32 = 0;
pub const VA_DISPLAY_ATTRIB_GETTABLE: u32 = 1;
pub const VA_DISPLAY_ATTRIB_SETTABLE: u32 = 2;
pub const VA_PICTURE_HEVC_INVALID: u32 = 1;
pub const VA_PICTURE_HEVC_FIELD_PIC: u32 = 2;
pub const VA_PICTURE_HEVC_BOTTOM_FIELD: u32 = 4;
pub const VA_PICTURE_HEVC_LONG_TERM_REFERENCE: u32 = 8;
pub const VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE: u32 = 16;
pub const VA_PICTURE_HEVC_RPS_ST_CURR_AFTER: u32 = 32;
pub const VA_PICTURE_HEVC_RPS_LT_CURR: u32 = 64;
pub const VA_PICTURE_VVC_INVALID: u32 = 1;
pub const VA_PICTURE_VVC_LONG_TERM_REFERENCE: u32 = 2;
pub const VA_PICTURE_VVC_UNAVAILABLE_REFERENCE: u32 = 4;
pub const VA_FEI_FUNCTION_ENC: u32 = 1;
pub const VA_FEI_FUNCTION_PAK: u32 = 2;
pub const VA_FEI_FUNCTION_ENC_PAK: u32 = 4;
pub const VA_PICTURE_STATS_INVALID: u32 = 1;
pub const VA_PICTURE_STATS_PROGRESSIVE: u32 = 0;
pub const VA_PICTURE_STATS_TOP_FIELD: u32 = 2;
pub const VA_PICTURE_STATS_BOTTOM_FIELD: u32 = 4;
pub const VA_PICTURE_STATS_CONTENT_UPDATED: u32 = 16;
pub const VA_MB_PRED_AVAIL_TOP_LEFT: u32 = 4;
pub const VA_MB_PRED_AVAIL_TOP: u32 = 16;
pub const VA_MB_PRED_AVAIL_TOP_RIGHT: u32 = 8;
pub const VA_MB_PRED_AVAIL_LEFT: u32 = 64;
pub const VA_AV1_MAX_SEGMENTS: u32 = 8;
pub const VA_AV1_SEG_LVL_MAX: u32 = 8;
pub const VA_BLEND_GLOBAL_ALPHA: u32 = 1;
pub const VA_BLEND_PREMULTIPLIED_ALPHA: u32 = 2;
pub const VA_BLEND_LUMA_KEY: u32 = 16;
pub const VA_PROC_PIPELINE_SUBPICTURES: u32 = 1;
pub const VA_PROC_PIPELINE_FAST: u32 = 2;
pub const VA_PROC_FILTER_MANDATORY: u32 = 1;
pub const VA_PIPELINE_FLAG_END: u32 = 4;
pub const VA_CHROMA_SITING_UNKNOWN: u32 = 0;
pub const VA_CHROMA_SITING_VERTICAL_TOP: u32 = 1;
pub const VA_CHROMA_SITING_VERTICAL_CENTER: u32 = 2;
pub const VA_CHROMA_SITING_VERTICAL_BOTTOM: u32 = 3;
pub const VA_CHROMA_SITING_HORIZONTAL_LEFT: u32 = 4;
pub const VA_CHROMA_SITING_HORIZONTAL_CENTER: u32 = 8;
pub const VA_SOURCE_RANGE_UNKNOWN: u32 = 0;
pub const VA_SOURCE_RANGE_REDUCED: u32 = 1;
pub const VA_SOURCE_RANGE_FULL: u32 = 2;
pub const VA_TONE_MAPPING_HDR_TO_HDR: u32 = 1;
pub const VA_TONE_MAPPING_HDR_TO_SDR: u32 = 2;
pub const VA_TONE_MAPPING_HDR_TO_EDR: u32 = 4;
pub const VA_TONE_MAPPING_SDR_TO_HDR: u32 = 8;
pub const VA_DEINTERLACING_BOTTOM_FIELD_FIRST: u32 = 1;
pub const VA_DEINTERLACING_BOTTOM_FIELD: u32 = 2;
pub const VA_DEINTERLACING_ONE_FIELD: u32 = 4;
pub const VA_DEINTERLACING_FMD_ENABLE: u32 = 8;
pub const VA_DEINTERLACING_SCD_ENABLE: u32 = 16;
pub const VA_PROC_HVS_DENOISE_DEFAULT: u32 = 0;
pub const VA_PROC_HVS_DENOISE_AUTO_BDRATE: u32 = 1;
pub const VA_PROC_HVS_DENOISE_AUTO_SUBJECTIVE: u32 = 2;
pub const VA_PROC_HVS_DENOISE_MANUAL: u32 = 3;
pub const VA_3DLUT_CHANNEL_UNKNOWN: u32 = 0;
pub const VA_3DLUT_CHANNEL_RGB_RGB: u32 = 1;
pub const VA_3DLUT_CHANNEL_YUV_RGB: u32 = 2;
pub const VA_3DLUT_CHANNEL_VUY_RGB: u32 = 4;
pub const VA_SURFACE_ATTRIB_MEM_TYPE_KERNEL_DRM: u32 = 268435456;
pub const VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME: u32 = 536870912;
pub const VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2: u32 = 1073741824;
pub const VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_3: u32 = 134217728;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _VARectangle {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VARectangle"][::core::mem::size_of::<_VARectangle>() - 8usize];
    ["Alignment of _VARectangle"][::core::mem::align_of::<_VARectangle>() - 2usize];
    ["Offset of field: _VARectangle::x"][::core::mem::offset_of!(_VARectangle, x) - 0usize];
    ["Offset of field: _VARectangle::y"][::core::mem::offset_of!(_VARectangle, y) - 2usize];
    ["Offset of field: _VARectangle::width"][::core::mem::offset_of!(_VARectangle, width) - 4usize];
    ["Offset of field: _VARectangle::height"]
        [::core::mem::offset_of!(_VARectangle, height) - 6usize];
};
pub type VARectangle = _VARectangle;
pub const VAConfigAttribType_VAConfigAttribRTFormat: VAConfigAttribType = 0;
pub const VAConfigAttribType_VAConfigAttribSpatialResidual: VAConfigAttribType = 1;
pub const VAConfigAttribType_VAConfigAttribSpatialClipping: VAConfigAttribType = 2;
pub const VAConfigAttribType_VAConfigAttribIntraResidual: VAConfigAttribType = 3;
pub const VAConfigAttribType_VAConfigAttribEncryption: VAConfigAttribType = 4;
pub const VAConfigAttribType_VAConfigAttribRateControl: VAConfigAttribType = 5;
pub const VAConfigAttribType_VAConfigAttribDecSliceMode: VAConfigAttribType = 6;
pub const VAConfigAttribType_VAConfigAttribDecJPEG: VAConfigAttribType = 7;
pub const VAConfigAttribType_VAConfigAttribDecProcessing: VAConfigAttribType = 8;
pub const VAConfigAttribType_VAConfigAttribEncPackedHeaders: VAConfigAttribType = 10;
pub const VAConfigAttribType_VAConfigAttribEncInterlaced: VAConfigAttribType = 11;
pub const VAConfigAttribType_VAConfigAttribEncMaxRefFrames: VAConfigAttribType = 13;
pub const VAConfigAttribType_VAConfigAttribEncMaxSlices: VAConfigAttribType = 14;
pub const VAConfigAttribType_VAConfigAttribEncSliceStructure: VAConfigAttribType = 15;
pub const VAConfigAttribType_VAConfigAttribEncMacroblockInfo: VAConfigAttribType = 16;
pub const VAConfigAttribType_VAConfigAttribMaxPictureWidth: VAConfigAttribType = 18;
pub const VAConfigAttribType_VAConfigAttribMaxPictureHeight: VAConfigAttribType = 19;
pub const VAConfigAttribType_VAConfigAttribEncJPEG: VAConfigAttribType = 20;
pub const VAConfigAttribType_VAConfigAttribEncQualityRange: VAConfigAttribType = 21;
pub const VAConfigAttribType_VAConfigAttribEncQuantization: VAConfigAttribType = 22;
pub const VAConfigAttribType_VAConfigAttribEncIntraRefresh: VAConfigAttribType = 23;
pub const VAConfigAttribType_VAConfigAttribEncSkipFrame: VAConfigAttribType = 24;
pub const VAConfigAttribType_VAConfigAttribEncROI: VAConfigAttribType = 25;
pub const VAConfigAttribType_VAConfigAttribEncRateControlExt: VAConfigAttribType = 26;
pub const VAConfigAttribType_VAConfigAttribProcessingRate: VAConfigAttribType = 27;
pub const VAConfigAttribType_VAConfigAttribEncDirtyRect: VAConfigAttribType = 28;
pub const VAConfigAttribType_VAConfigAttribEncParallelRateControl: VAConfigAttribType = 29;
pub const VAConfigAttribType_VAConfigAttribEncDynamicScaling: VAConfigAttribType = 30;
pub const VAConfigAttribType_VAConfigAttribFrameSizeToleranceSupport: VAConfigAttribType = 31;
pub const VAConfigAttribType_VAConfigAttribFEIFunctionType: VAConfigAttribType = 32;
pub const VAConfigAttribType_VAConfigAttribFEIMVPredictors: VAConfigAttribType = 33;
pub const VAConfigAttribType_VAConfigAttribStats: VAConfigAttribType = 34;
pub const VAConfigAttribType_VAConfigAttribEncTileSupport: VAConfigAttribType = 35;
pub const VAConfigAttribType_VAConfigAttribCustomRoundingControl: VAConfigAttribType = 36;
pub const VAConfigAttribType_VAConfigAttribQPBlockSize: VAConfigAttribType = 37;
pub const VAConfigAttribType_VAConfigAttribMaxFrameSize: VAConfigAttribType = 38;
pub const VAConfigAttribType_VAConfigAttribPredictionDirection: VAConfigAttribType = 39;
pub const VAConfigAttribType_VAConfigAttribMultipleFrame: VAConfigAttribType = 40;
pub const VAConfigAttribType_VAConfigAttribContextPriority: VAConfigAttribType = 41;
pub const VAConfigAttribType_VAConfigAttribDecAV1Features: VAConfigAttribType = 42;
pub const VAConfigAttribType_VAConfigAttribTEEType: VAConfigAttribType = 43;
pub const VAConfigAttribType_VAConfigAttribTEETypeClient: VAConfigAttribType = 44;
pub const VAConfigAttribType_VAConfigAttribProtectedContentCipherAlgorithm: VAConfigAttribType = 45;
pub const VAConfigAttribType_VAConfigAttribProtectedContentCipherBlockSize: VAConfigAttribType = 46;
pub const VAConfigAttribType_VAConfigAttribProtectedContentCipherMode: VAConfigAttribType = 47;
pub const VAConfigAttribType_VAConfigAttribProtectedContentCipherSampleType: VAConfigAttribType =
    48;
pub const VAConfigAttribType_VAConfigAttribProtectedContentUsage: VAConfigAttribType = 49;
pub const VAConfigAttribType_VAConfigAttribEncHEVCFeatures: VAConfigAttribType = 50;
pub const VAConfigAttribType_VAConfigAttribEncHEVCBlockSizes: VAConfigAttribType = 51;
pub const VAConfigAttribType_VAConfigAttribEncAV1: VAConfigAttribType = 52;
pub const VAConfigAttribType_VAConfigAttribEncAV1Ext1: VAConfigAttribType = 53;
pub const VAConfigAttribType_VAConfigAttribEncAV1Ext2: VAConfigAttribType = 54;
pub const VAConfigAttribType_VAConfigAttribEncPerBlockControl: VAConfigAttribType = 55;
pub const VAConfigAttribType_VAConfigAttribEncMaxTileRows: VAConfigAttribType = 56;
pub const VAConfigAttribType_VAConfigAttribEncMaxTileCols: VAConfigAttribType = 57;
pub const VAConfigAttribType_VAConfigAttribTypeMax: VAConfigAttribType = 58;
pub type VAConfigAttribType = core::ffi::c_uint;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _VAConfigAttrib {
    pub type_: VAConfigAttribType,
    pub value: u32,
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAConfigAttrib"][::core::mem::size_of::<_VAConfigAttrib>() - 8usize];
    ["Alignment of _VAConfigAttrib"][::core::mem::align_of::<_VAConfigAttrib>() - 4usize];
    ["Offset of field: _VAConfigAttrib::type_"]
        [::core::mem::offset_of!(_VAConfigAttrib, type_) - 0usize];
    ["Offset of field: _VAConfigAttrib::value"]
        [::core::mem::offset_of!(_VAConfigAttrib, value) - 4usize];
};
pub type VAConfigAttrib = _VAConfigAttrib;
pub type VAGenericID = core::ffi::c_uint;
pub type VASurfaceID = VAGenericID;
pub const VAGenericValueType_VAGenericValueTypeInteger: VAGenericValueType = 1;
pub const VAGenericValueType_VAGenericValueTypeFloat: VAGenericValueType = 2;
pub const VAGenericValueType_VAGenericValueTypePointer: VAGenericValueType = 3;
pub const VAGenericValueType_VAGenericValueTypeFunc: VAGenericValueType = 4;
pub type VAGenericValueType = core::ffi::c_uint;
pub type VAGenericFunc = ::core::option::Option<unsafe extern "C" fn()>;
#[repr(C)]
#[derive(Copy, Clone)]
pub struct _VAGenericValue {
    pub type_: VAGenericValueType,
    pub value: _VAGenericValue__bindgen_ty_1,
}
#[repr(C)]
#[derive(Copy, Clone)]
pub union _VAGenericValue__bindgen_ty_1 {
    pub i: i32,
    pub f: f32,
    pub p: *mut core::ffi::c_void,
    pub fn_: VAGenericFunc,
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAGenericValue__bindgen_ty_1"]
        [::core::mem::size_of::<_VAGenericValue__bindgen_ty_1>() - 8usize];
    ["Alignment of _VAGenericValue__bindgen_ty_1"]
        [::core::mem::align_of::<_VAGenericValue__bindgen_ty_1>() - 8usize];
    ["Offset of field: _VAGenericValue__bindgen_ty_1::i"]
        [::core::mem::offset_of!(_VAGenericValue__bindgen_ty_1, i) - 0usize];
    ["Offset of field: _VAGenericValue__bindgen_ty_1::f"]
        [::core::mem::offset_of!(_VAGenericValue__bindgen_ty_1, f) - 0usize];
    ["Offset of field: _VAGenericValue__bindgen_ty_1::p"]
        [::core::mem::offset_of!(_VAGenericValue__bindgen_ty_1, p) - 0usize];
    ["Offset of field: _VAGenericValue__bindgen_ty_1::fn_"]
        [::core::mem::offset_of!(_VAGenericValue__bindgen_ty_1, fn_) - 0usize];
};
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAGenericValue"][::core::mem::size_of::<_VAGenericValue>() - 16usize];
    ["Alignment of _VAGenericValue"][::core::mem::align_of::<_VAGenericValue>() - 8usize];
    ["Offset of field: _VAGenericValue::type_"]
        [::core::mem::offset_of!(_VAGenericValue, type_) - 0usize];
    ["Offset of field: _VAGenericValue::value"]
        [::core::mem::offset_of!(_VAGenericValue, value) - 8usize];
};
pub type VAGenericValue = _VAGenericValue;
pub const VASurfaceAttribType_VASurfaceAttribNone: VASurfaceAttribType = 0;
pub const VASurfaceAttribType_VASurfaceAttribPixelFormat: VASurfaceAttribType = 1;
pub const VASurfaceAttribType_VASurfaceAttribMinWidth: VASurfaceAttribType = 2;
pub const VASurfaceAttribType_VASurfaceAttribMaxWidth: VASurfaceAttribType = 3;
pub const VASurfaceAttribType_VASurfaceAttribMinHeight: VASurfaceAttribType = 4;
pub const VASurfaceAttribType_VASurfaceAttribMaxHeight: VASurfaceAttribType = 5;
pub const VASurfaceAttribType_VASurfaceAttribMemoryType: VASurfaceAttribType = 6;
pub const VASurfaceAttribType_VASurfaceAttribExternalBufferDescriptor: VASurfaceAttribType = 7;
pub const VASurfaceAttribType_VASurfaceAttribUsageHint: VASurfaceAttribType = 8;
pub const VASurfaceAttribType_VASurfaceAttribDRMFormatModifiers: VASurfaceAttribType = 9;
pub const VASurfaceAttribType_VASurfaceAttribAlignmentSize: VASurfaceAttribType = 10;
pub const VASurfaceAttribType_VASurfaceAttribCount: VASurfaceAttribType = 11;
pub type VASurfaceAttribType = core::ffi::c_uint;
#[repr(C)]
#[derive(Copy, Clone)]
pub struct _VASurfaceAttrib {
    pub type_: VASurfaceAttribType,
    pub flags: u32,
    pub value: VAGenericValue,
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VASurfaceAttrib"][::core::mem::size_of::<_VASurfaceAttrib>() - 24usize];
    ["Alignment of _VASurfaceAttrib"][::core::mem::align_of::<_VASurfaceAttrib>() - 8usize];
    ["Offset of field: _VASurfaceAttrib::type_"]
        [::core::mem::offset_of!(_VASurfaceAttrib, type_) - 0usize];
    ["Offset of field: _VASurfaceAttrib::flags"]
        [::core::mem::offset_of!(_VASurfaceAttrib, flags) - 4usize];
    ["Offset of field: _VASurfaceAttrib::value"]
        [::core::mem::offset_of!(_VASurfaceAttrib, value) - 8usize];
};
pub type VASurfaceAttrib = _VASurfaceAttrib;
pub type VABufferID = VAGenericID;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeFrameRate: VAEncMiscParameterType = 0;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeRateControl: VAEncMiscParameterType = 1;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeMaxSliceSize: VAEncMiscParameterType = 2;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeAIR: VAEncMiscParameterType = 3;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeMaxFrameSize: VAEncMiscParameterType = 4;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeHRD: VAEncMiscParameterType = 5;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeQualityLevel: VAEncMiscParameterType = 6;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeRIR: VAEncMiscParameterType = 7;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeQuantization: VAEncMiscParameterType = 8;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeSkipFrame: VAEncMiscParameterType = 9;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeROI: VAEncMiscParameterType = 10;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeMultiPassFrameSize: VAEncMiscParameterType =
    11;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeTemporalLayerStructure:
    VAEncMiscParameterType = 12;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeDirtyRect: VAEncMiscParameterType = 13;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeParallelBRC: VAEncMiscParameterType = 14;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeSubMbPartPel: VAEncMiscParameterType = 15;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeEncQuality: VAEncMiscParameterType = 16;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeCustomRoundingControl:
    VAEncMiscParameterType = 17;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeFEIFrameControl: VAEncMiscParameterType = 18;
pub const VAEncMiscParameterType_VAEncMiscParameterTypeExtensionData: VAEncMiscParameterType = 19;
pub type VAEncMiscParameterType = core::ffi::c_uint;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _VAEncPackedHeaderParameterBuffer {
    pub type_: u32,
    pub bit_length: u32,
    pub has_emulation_bytes: u8,
    pub va_reserved: [u32; 4usize],
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncPackedHeaderParameterBuffer"]
        [::core::mem::size_of::<_VAEncPackedHeaderParameterBuffer>() - 28usize];
    ["Alignment of _VAEncPackedHeaderParameterBuffer"]
        [::core::mem::align_of::<_VAEncPackedHeaderParameterBuffer>() - 4usize];
    ["Offset of field: _VAEncPackedHeaderParameterBuffer::type_"]
        [::core::mem::offset_of!(_VAEncPackedHeaderParameterBuffer, type_) - 0usize];
    ["Offset of field: _VAEncPackedHeaderParameterBuffer::bit_length"]
        [::core::mem::offset_of!(_VAEncPackedHeaderParameterBuffer, bit_length) - 4usize];
    ["Offset of field: _VAEncPackedHeaderParameterBuffer::has_emulation_bytes"]
        [::core::mem::offset_of!(_VAEncPackedHeaderParameterBuffer, has_emulation_bytes) - 8usize];
    ["Offset of field: _VAEncPackedHeaderParameterBuffer::va_reserved"]
        [::core::mem::offset_of!(_VAEncPackedHeaderParameterBuffer, va_reserved) - 12usize];
};
pub type VAEncPackedHeaderParameterBuffer = _VAEncPackedHeaderParameterBuffer;
#[repr(C)]
#[derive(Debug)]
pub struct _VAEncMiscParameterBuffer {
    pub type_: VAEncMiscParameterType,
    pub data: __IncompleteArrayField<u32>,
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncMiscParameterBuffer"]
        [::core::mem::size_of::<_VAEncMiscParameterBuffer>() - 4usize];
    ["Alignment of _VAEncMiscParameterBuffer"]
        [::core::mem::align_of::<_VAEncMiscParameterBuffer>() - 4usize];
    ["Offset of field: _VAEncMiscParameterBuffer::type_"]
        [::core::mem::offset_of!(_VAEncMiscParameterBuffer, type_) - 0usize];
    ["Offset of field: _VAEncMiscParameterBuffer::data"]
        [::core::mem::offset_of!(_VAEncMiscParameterBuffer, data) - 4usize];
};
pub type VAEncMiscParameterBuffer = _VAEncMiscParameterBuffer;
#[repr(C)]
#[derive(Copy, Clone)]
pub struct _VAEncMiscParameterRateControl {
    pub bits_per_second: u32,
    pub target_percentage: u32,
    pub window_size: u32,
    pub initial_qp: u32,
    pub min_qp: u32,
    pub basic_unit_size: u32,
    pub rc_flags: _VAEncMiscParameterRateControl__bindgen_ty_1,
    pub ICQ_quality_factor: u32,
    pub max_qp: u32,
    pub quality_factor: u32,
    pub target_frame_size: u32,
    pub va_reserved: [u32; 4usize],
}
#[repr(C)]
#[derive(Copy, Clone)]
pub union _VAEncMiscParameterRateControl__bindgen_ty_1 {
    pub bits: _VAEncMiscParameterRateControl__bindgen_ty_1__bindgen_ty_1,
    pub value: u32,
}
#[repr(C)]
#[repr(align(4))]
#[derive(Debug, Copy, Clone)]
pub struct _VAEncMiscParameterRateControl__bindgen_ty_1__bindgen_ty_1 {
    pub _bitfield_align_1: [u16; 0],
    pub _bitfield_1: __BindgenBitfieldUnit<[u8; 4usize]>,
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncMiscParameterRateControl__bindgen_ty_1__bindgen_ty_1"][::core::mem::size_of::<
        _VAEncMiscParameterRateControl__bindgen_ty_1__bindgen_ty_1,
    >() - 4usize];
    ["Alignment of _VAEncMiscParameterRateControl__bindgen_ty_1__bindgen_ty_1"]
        [::core::mem::align_of::<_VAEncMiscParameterRateControl__bindgen_ty_1__bindgen_ty_1>()
            - 4usize];
};
impl _VAEncMiscParameterRateControl__bindgen_ty_1__bindgen_ty_1 {
    #[inline]
    pub fn reset(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(0usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_reset(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(0usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn reset_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                0usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_reset_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                0usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn disable_frame_skip(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(1usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_disable_frame_skip(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(1usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn disable_frame_skip_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                1usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_disable_frame_skip_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                1usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn disable_bit_stuffing(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(2usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_disable_bit_stuffing(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(2usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn disable_bit_stuffing_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                2usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_disable_bit_stuffing_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                2usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn mb_rate_control(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(3usize, 4u8) as u32) }
    }
    #[inline]
    pub fn set_mb_rate_control(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(3usize, 4u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn mb_rate_control_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                3usize,
                4u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_mb_rate_control_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                3usize,
                4u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn temporal_id(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(7usize, 8u8) as u32) }
    }
    #[inline]
    pub fn set_temporal_id(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(7usize, 8u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn temporal_id_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                7usize,
                8u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_temporal_id_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                7usize,
                8u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn cfs_I_frames(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(15usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_cfs_I_frames(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(15usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn cfs_I_frames_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                15usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_cfs_I_frames_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                15usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn enable_parallel_brc(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(16usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_enable_parallel_brc(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(16usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn enable_parallel_brc_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                16usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_enable_parallel_brc_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                16usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn enable_dynamic_scaling(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(17usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_enable_dynamic_scaling(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(17usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn enable_dynamic_scaling_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                17usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_enable_dynamic_scaling_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                17usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn frame_tolerance_mode(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(18usize, 2u8) as u32) }
    }
    #[inline]
    pub fn set_frame_tolerance_mode(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(18usize, 2u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn frame_tolerance_mode_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                18usize,
                2u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_frame_tolerance_mode_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                18usize,
                2u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn reserved(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(20usize, 12u8) as u32) }
    }
    #[inline]
    pub fn set_reserved(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(20usize, 12u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn reserved_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                20usize,
                12u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_reserved_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                20usize,
                12u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn new_bitfield_1(
        reset: u32,
        disable_frame_skip: u32,
        disable_bit_stuffing: u32,
        mb_rate_control: u32,
        temporal_id: u32,
        cfs_I_frames: u32,
        enable_parallel_brc: u32,
        enable_dynamic_scaling: u32,
        frame_tolerance_mode: u32,
        reserved: u32,
    ) -> __BindgenBitfieldUnit<[u8; 4usize]> {
        let mut __bindgen_bitfield_unit: __BindgenBitfieldUnit<[u8; 4usize]> = Default::default();
        __bindgen_bitfield_unit.set(0usize, 1u8, {
            let reset: u32 = unsafe { ::core::mem::transmute(reset) };
            reset as u64
        });
        __bindgen_bitfield_unit.set(1usize, 1u8, {
            let disable_frame_skip: u32 = unsafe { ::core::mem::transmute(disable_frame_skip) };
            disable_frame_skip as u64
        });
        __bindgen_bitfield_unit.set(2usize, 1u8, {
            let disable_bit_stuffing: u32 = unsafe { ::core::mem::transmute(disable_bit_stuffing) };
            disable_bit_stuffing as u64
        });
        __bindgen_bitfield_unit.set(3usize, 4u8, {
            let mb_rate_control: u32 = unsafe { ::core::mem::transmute(mb_rate_control) };
            mb_rate_control as u64
        });
        __bindgen_bitfield_unit.set(7usize, 8u8, {
            let temporal_id: u32 = unsafe { ::core::mem::transmute(temporal_id) };
            temporal_id as u64
        });
        __bindgen_bitfield_unit.set(15usize, 1u8, {
            let cfs_I_frames: u32 = unsafe { ::core::mem::transmute(cfs_I_frames) };
            cfs_I_frames as u64
        });
        __bindgen_bitfield_unit.set(16usize, 1u8, {
            let enable_parallel_brc: u32 = unsafe { ::core::mem::transmute(enable_parallel_brc) };
            enable_parallel_brc as u64
        });
        __bindgen_bitfield_unit.set(17usize, 1u8, {
            let enable_dynamic_scaling: u32 =
                unsafe { ::core::mem::transmute(enable_dynamic_scaling) };
            enable_dynamic_scaling as u64
        });
        __bindgen_bitfield_unit.set(18usize, 2u8, {
            let frame_tolerance_mode: u32 = unsafe { ::core::mem::transmute(frame_tolerance_mode) };
            frame_tolerance_mode as u64
        });
        __bindgen_bitfield_unit.set(20usize, 12u8, {
            let reserved: u32 = unsafe { ::core::mem::transmute(reserved) };
            reserved as u64
        });
        __bindgen_bitfield_unit
    }
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncMiscParameterRateControl__bindgen_ty_1"]
        [::core::mem::size_of::<_VAEncMiscParameterRateControl__bindgen_ty_1>() - 4usize];
    ["Alignment of _VAEncMiscParameterRateControl__bindgen_ty_1"]
        [::core::mem::align_of::<_VAEncMiscParameterRateControl__bindgen_ty_1>() - 4usize];
    ["Offset of field: _VAEncMiscParameterRateControl__bindgen_ty_1::bits"]
        [::core::mem::offset_of!(_VAEncMiscParameterRateControl__bindgen_ty_1, bits) - 0usize];
    ["Offset of field: _VAEncMiscParameterRateControl__bindgen_ty_1::value"]
        [::core::mem::offset_of!(_VAEncMiscParameterRateControl__bindgen_ty_1, value) - 0usize];
};
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncMiscParameterRateControl"]
        [::core::mem::size_of::<_VAEncMiscParameterRateControl>() - 60usize];
    ["Alignment of _VAEncMiscParameterRateControl"]
        [::core::mem::align_of::<_VAEncMiscParameterRateControl>() - 4usize];
    ["Offset of field: _VAEncMiscParameterRateControl::bits_per_second"]
        [::core::mem::offset_of!(_VAEncMiscParameterRateControl, bits_per_second) - 0usize];
    ["Offset of field: _VAEncMiscParameterRateControl::target_percentage"]
        [::core::mem::offset_of!(_VAEncMiscParameterRateControl, target_percentage) - 4usize];
    ["Offset of field: _VAEncMiscParameterRateControl::window_size"]
        [::core::mem::offset_of!(_VAEncMiscParameterRateControl, window_size) - 8usize];
    ["Offset of field: _VAEncMiscParameterRateControl::initial_qp"]
        [::core::mem::offset_of!(_VAEncMiscParameterRateControl, initial_qp) - 12usize];
    ["Offset of field: _VAEncMiscParameterRateControl::min_qp"]
        [::core::mem::offset_of!(_VAEncMiscParameterRateControl, min_qp) - 16usize];
    ["Offset of field: _VAEncMiscParameterRateControl::basic_unit_size"]
        [::core::mem::offset_of!(_VAEncMiscParameterRateControl, basic_unit_size) - 20usize];
    ["Offset of field: _VAEncMiscParameterRateControl::rc_flags"]
        [::core::mem::offset_of!(_VAEncMiscParameterRateControl, rc_flags) - 24usize];
    ["Offset of field: _VAEncMiscParameterRateControl::ICQ_quality_factor"]
        [::core::mem::offset_of!(_VAEncMiscParameterRateControl, ICQ_quality_factor) - 28usize];
    ["Offset of field: _VAEncMiscParameterRateControl::max_qp"]
        [::core::mem::offset_of!(_VAEncMiscParameterRateControl, max_qp) - 32usize];
    ["Offset of field: _VAEncMiscParameterRateControl::quality_factor"]
        [::core::mem::offset_of!(_VAEncMiscParameterRateControl, quality_factor) - 36usize];
    ["Offset of field: _VAEncMiscParameterRateControl::target_frame_size"]
        [::core::mem::offset_of!(_VAEncMiscParameterRateControl, target_frame_size) - 40usize];
    ["Offset of field: _VAEncMiscParameterRateControl::va_reserved"]
        [::core::mem::offset_of!(_VAEncMiscParameterRateControl, va_reserved) - 44usize];
};
pub type VAEncMiscParameterRateControl = _VAEncMiscParameterRateControl;
#[repr(C)]
#[derive(Copy, Clone)]
pub struct _VAEncMiscParameterFrameRate {
    pub framerate: u32,
    pub framerate_flags: _VAEncMiscParameterFrameRate__bindgen_ty_1,
    pub va_reserved: [u32; 4usize],
}
#[repr(C)]
#[derive(Copy, Clone)]
pub union _VAEncMiscParameterFrameRate__bindgen_ty_1 {
    pub bits: _VAEncMiscParameterFrameRate__bindgen_ty_1__bindgen_ty_1,
    pub value: u32,
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _VAEncMiscParameterFrameRate__bindgen_ty_1__bindgen_ty_1 {
    pub _bitfield_align_1: [u32; 0],
    pub _bitfield_1: __BindgenBitfieldUnit<[u8; 4usize]>,
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncMiscParameterFrameRate__bindgen_ty_1__bindgen_ty_1"][::core::mem::size_of::<
        _VAEncMiscParameterFrameRate__bindgen_ty_1__bindgen_ty_1,
    >() - 4usize];
    ["Alignment of _VAEncMiscParameterFrameRate__bindgen_ty_1__bindgen_ty_1"][::core::mem::align_of::<
        _VAEncMiscParameterFrameRate__bindgen_ty_1__bindgen_ty_1,
    >() - 4usize];
};
impl _VAEncMiscParameterFrameRate__bindgen_ty_1__bindgen_ty_1 {
    #[inline]
    pub fn temporal_id(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(0usize, 8u8) as u32) }
    }
    #[inline]
    pub fn set_temporal_id(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(0usize, 8u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn temporal_id_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                0usize,
                8u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_temporal_id_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                0usize,
                8u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn reserved(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(8usize, 24u8) as u32) }
    }
    #[inline]
    pub fn set_reserved(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(8usize, 24u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn reserved_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                8usize,
                24u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_reserved_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                8usize,
                24u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn new_bitfield_1(temporal_id: u32, reserved: u32) -> __BindgenBitfieldUnit<[u8; 4usize]> {
        let mut __bindgen_bitfield_unit: __BindgenBitfieldUnit<[u8; 4usize]> = Default::default();
        __bindgen_bitfield_unit.set(0usize, 8u8, {
            let temporal_id: u32 = unsafe { ::core::mem::transmute(temporal_id) };
            temporal_id as u64
        });
        __bindgen_bitfield_unit.set(8usize, 24u8, {
            let reserved: u32 = unsafe { ::core::mem::transmute(reserved) };
            reserved as u64
        });
        __bindgen_bitfield_unit
    }
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncMiscParameterFrameRate__bindgen_ty_1"]
        [::core::mem::size_of::<_VAEncMiscParameterFrameRate__bindgen_ty_1>() - 4usize];
    ["Alignment of _VAEncMiscParameterFrameRate__bindgen_ty_1"]
        [::core::mem::align_of::<_VAEncMiscParameterFrameRate__bindgen_ty_1>() - 4usize];
    ["Offset of field: _VAEncMiscParameterFrameRate__bindgen_ty_1::bits"]
        [::core::mem::offset_of!(_VAEncMiscParameterFrameRate__bindgen_ty_1, bits) - 0usize];
    ["Offset of field: _VAEncMiscParameterFrameRate__bindgen_ty_1::value"]
        [::core::mem::offset_of!(_VAEncMiscParameterFrameRate__bindgen_ty_1, value) - 0usize];
};
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncMiscParameterFrameRate"]
        [::core::mem::size_of::<_VAEncMiscParameterFrameRate>() - 24usize];
    ["Alignment of _VAEncMiscParameterFrameRate"]
        [::core::mem::align_of::<_VAEncMiscParameterFrameRate>() - 4usize];
    ["Offset of field: _VAEncMiscParameterFrameRate::framerate"]
        [::core::mem::offset_of!(_VAEncMiscParameterFrameRate, framerate) - 0usize];
    ["Offset of field: _VAEncMiscParameterFrameRate::framerate_flags"]
        [::core::mem::offset_of!(_VAEncMiscParameterFrameRate, framerate_flags) - 4usize];
    ["Offset of field: _VAEncMiscParameterFrameRate::va_reserved"]
        [::core::mem::offset_of!(_VAEncMiscParameterFrameRate, va_reserved) - 8usize];
};
pub type VAEncMiscParameterFrameRate = _VAEncMiscParameterFrameRate;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _VAEncMiscParameterHRD {
    pub initial_buffer_fullness: u32,
    pub buffer_size: u32,
    pub va_reserved: [u32; 4usize],
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncMiscParameterHRD"][::core::mem::size_of::<_VAEncMiscParameterHRD>() - 24usize];
    ["Alignment of _VAEncMiscParameterHRD"]
        [::core::mem::align_of::<_VAEncMiscParameterHRD>() - 4usize];
    ["Offset of field: _VAEncMiscParameterHRD::initial_buffer_fullness"]
        [::core::mem::offset_of!(_VAEncMiscParameterHRD, initial_buffer_fullness) - 0usize];
    ["Offset of field: _VAEncMiscParameterHRD::buffer_size"]
        [::core::mem::offset_of!(_VAEncMiscParameterHRD, buffer_size) - 4usize];
    ["Offset of field: _VAEncMiscParameterHRD::va_reserved"]
        [::core::mem::offset_of!(_VAEncMiscParameterHRD, va_reserved) - 8usize];
};
pub type VAEncMiscParameterHRD = _VAEncMiscParameterHRD;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _VAPictureH264 {
    pub picture_id: VASurfaceID,
    pub frame_idx: u32,
    pub flags: u32,
    pub TopFieldOrderCnt: i32,
    pub BottomFieldOrderCnt: i32,
    pub va_reserved: [u32; 4usize],
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAPictureH264"][::core::mem::size_of::<_VAPictureH264>() - 36usize];
    ["Alignment of _VAPictureH264"][::core::mem::align_of::<_VAPictureH264>() - 4usize];
    ["Offset of field: _VAPictureH264::picture_id"]
        [::core::mem::offset_of!(_VAPictureH264, picture_id) - 0usize];
    ["Offset of field: _VAPictureH264::frame_idx"]
        [::core::mem::offset_of!(_VAPictureH264, frame_idx) - 4usize];
    ["Offset of field: _VAPictureH264::flags"]
        [::core::mem::offset_of!(_VAPictureH264, flags) - 8usize];
    ["Offset of field: _VAPictureH264::TopFieldOrderCnt"]
        [::core::mem::offset_of!(_VAPictureH264, TopFieldOrderCnt) - 12usize];
    ["Offset of field: _VAPictureH264::BottomFieldOrderCnt"]
        [::core::mem::offset_of!(_VAPictureH264, BottomFieldOrderCnt) - 16usize];
    ["Offset of field: _VAPictureH264::va_reserved"]
        [::core::mem::offset_of!(_VAPictureH264, va_reserved) - 20usize];
};
pub type VAPictureH264 = _VAPictureH264;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _VACodedBufferSegment {
    pub size: u32,
    pub bit_offset: u32,
    pub status: u32,
    pub reserved: u32,
    pub buf: *mut core::ffi::c_void,
    pub next: *mut core::ffi::c_void,
    pub va_reserved: [u32; 4usize],
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VACodedBufferSegment"][::core::mem::size_of::<_VACodedBufferSegment>() - 48usize];
    ["Alignment of _VACodedBufferSegment"]
        [::core::mem::align_of::<_VACodedBufferSegment>() - 8usize];
    ["Offset of field: _VACodedBufferSegment::size"]
        [::core::mem::offset_of!(_VACodedBufferSegment, size) - 0usize];
    ["Offset of field: _VACodedBufferSegment::bit_offset"]
        [::core::mem::offset_of!(_VACodedBufferSegment, bit_offset) - 4usize];
    ["Offset of field: _VACodedBufferSegment::status"]
        [::core::mem::offset_of!(_VACodedBufferSegment, status) - 8usize];
    ["Offset of field: _VACodedBufferSegment::reserved"]
        [::core::mem::offset_of!(_VACodedBufferSegment, reserved) - 12usize];
    ["Offset of field: _VACodedBufferSegment::buf"]
        [::core::mem::offset_of!(_VACodedBufferSegment, buf) - 16usize];
    ["Offset of field: _VACodedBufferSegment::next"]
        [::core::mem::offset_of!(_VACodedBufferSegment, next) - 24usize];
    ["Offset of field: _VACodedBufferSegment::va_reserved"]
        [::core::mem::offset_of!(_VACodedBufferSegment, va_reserved) - 32usize];
};
pub type VACodedBufferSegment = _VACodedBufferSegment;
#[repr(C)]
#[derive(Copy, Clone)]
pub struct _VAEncSequenceParameterBufferH264 {
    pub seq_parameter_set_id: u8,
    pub level_idc: u8,
    pub intra_period: u32,
    pub intra_idr_period: u32,
    pub ip_period: u32,
    pub bits_per_second: u32,
    pub max_num_ref_frames: u32,
    pub picture_width_in_mbs: u16,
    pub picture_height_in_mbs: u16,
    pub seq_fields: _VAEncSequenceParameterBufferH264__bindgen_ty_1,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
    pub num_ref_frames_in_pic_order_cnt_cycle: u8,
    pub offset_for_non_ref_pic: i32,
    pub offset_for_top_to_bottom_field: i32,
    pub offset_for_ref_frame: [i32; 256usize],
    pub frame_cropping_flag: u8,
    pub frame_crop_left_offset: u32,
    pub frame_crop_right_offset: u32,
    pub frame_crop_top_offset: u32,
    pub frame_crop_bottom_offset: u32,
    pub vui_parameters_present_flag: u8,
    pub vui_fields: _VAEncSequenceParameterBufferH264__bindgen_ty_2,
    pub aspect_ratio_idc: u8,
    pub sar_width: u32,
    pub sar_height: u32,
    pub num_units_in_tick: u32,
    pub time_scale: u32,
    pub va_reserved: [u32; 4usize],
}
#[repr(C)]
#[derive(Copy, Clone)]
pub union _VAEncSequenceParameterBufferH264__bindgen_ty_1 {
    pub bits: _VAEncSequenceParameterBufferH264__bindgen_ty_1__bindgen_ty_1,
    pub value: u32,
}
#[repr(C)]
#[repr(align(4))]
#[derive(Debug, Copy, Clone)]
pub struct _VAEncSequenceParameterBufferH264__bindgen_ty_1__bindgen_ty_1 {
    pub _bitfield_align_1: [u8; 0],
    pub _bitfield_1: __BindgenBitfieldUnit<[u8; 3usize]>,
    pub __bindgen_padding_0: u8,
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncSequenceParameterBufferH264__bindgen_ty_1__bindgen_ty_1"][::core::mem::size_of::<
        _VAEncSequenceParameterBufferH264__bindgen_ty_1__bindgen_ty_1,
    >() - 4usize];
    ["Alignment of _VAEncSequenceParameterBufferH264__bindgen_ty_1__bindgen_ty_1"]
        [::core::mem::align_of::<_VAEncSequenceParameterBufferH264__bindgen_ty_1__bindgen_ty_1>()
            - 4usize];
};
impl _VAEncSequenceParameterBufferH264__bindgen_ty_1__bindgen_ty_1 {
    #[inline]
    pub fn chroma_format_idc(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(0usize, 2u8) as u32) }
    }
    #[inline]
    pub fn set_chroma_format_idc(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(0usize, 2u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn chroma_format_idc_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 3usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                0usize,
                2u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_chroma_format_idc_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 3usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                0usize,
                2u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn frame_mbs_only_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(2usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_frame_mbs_only_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(2usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn frame_mbs_only_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 3usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                2usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_frame_mbs_only_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 3usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                2usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn mb_adaptive_frame_field_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(3usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_mb_adaptive_frame_field_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(3usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn mb_adaptive_frame_field_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 3usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                3usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_mb_adaptive_frame_field_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 3usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                3usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn seq_scaling_matrix_present_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(4usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_seq_scaling_matrix_present_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(4usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn seq_scaling_matrix_present_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 3usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                4usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_seq_scaling_matrix_present_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 3usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                4usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn direct_8x8_inference_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(5usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_direct_8x8_inference_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(5usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn direct_8x8_inference_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 3usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                5usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_direct_8x8_inference_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 3usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                5usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn log2_max_frame_num_minus4(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(6usize, 4u8) as u32) }
    }
    #[inline]
    pub fn set_log2_max_frame_num_minus4(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(6usize, 4u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn log2_max_frame_num_minus4_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 3usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                6usize,
                4u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_log2_max_frame_num_minus4_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 3usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                6usize,
                4u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn pic_order_cnt_type(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(10usize, 2u8) as u32) }
    }
    #[inline]
    pub fn set_pic_order_cnt_type(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(10usize, 2u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn pic_order_cnt_type_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 3usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                10usize,
                2u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_pic_order_cnt_type_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 3usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                10usize,
                2u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn log2_max_pic_order_cnt_lsb_minus4(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(12usize, 4u8) as u32) }
    }
    #[inline]
    pub fn set_log2_max_pic_order_cnt_lsb_minus4(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(12usize, 4u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn log2_max_pic_order_cnt_lsb_minus4_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 3usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                12usize,
                4u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_log2_max_pic_order_cnt_lsb_minus4_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 3usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                12usize,
                4u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn delta_pic_order_always_zero_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(16usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_delta_pic_order_always_zero_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(16usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn delta_pic_order_always_zero_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 3usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                16usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_delta_pic_order_always_zero_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 3usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                16usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn new_bitfield_1(
        chroma_format_idc: u32,
        frame_mbs_only_flag: u32,
        mb_adaptive_frame_field_flag: u32,
        seq_scaling_matrix_present_flag: u32,
        direct_8x8_inference_flag: u32,
        log2_max_frame_num_minus4: u32,
        pic_order_cnt_type: u32,
        log2_max_pic_order_cnt_lsb_minus4: u32,
        delta_pic_order_always_zero_flag: u32,
    ) -> __BindgenBitfieldUnit<[u8; 3usize]> {
        let mut __bindgen_bitfield_unit: __BindgenBitfieldUnit<[u8; 3usize]> = Default::default();
        __bindgen_bitfield_unit.set(0usize, 2u8, {
            let chroma_format_idc: u32 = unsafe { ::core::mem::transmute(chroma_format_idc) };
            chroma_format_idc as u64
        });
        __bindgen_bitfield_unit.set(2usize, 1u8, {
            let frame_mbs_only_flag: u32 = unsafe { ::core::mem::transmute(frame_mbs_only_flag) };
            frame_mbs_only_flag as u64
        });
        __bindgen_bitfield_unit.set(3usize, 1u8, {
            let mb_adaptive_frame_field_flag: u32 =
                unsafe { ::core::mem::transmute(mb_adaptive_frame_field_flag) };
            mb_adaptive_frame_field_flag as u64
        });
        __bindgen_bitfield_unit.set(4usize, 1u8, {
            let seq_scaling_matrix_present_flag: u32 =
                unsafe { ::core::mem::transmute(seq_scaling_matrix_present_flag) };
            seq_scaling_matrix_present_flag as u64
        });
        __bindgen_bitfield_unit.set(5usize, 1u8, {
            let direct_8x8_inference_flag: u32 =
                unsafe { ::core::mem::transmute(direct_8x8_inference_flag) };
            direct_8x8_inference_flag as u64
        });
        __bindgen_bitfield_unit.set(6usize, 4u8, {
            let log2_max_frame_num_minus4: u32 =
                unsafe { ::core::mem::transmute(log2_max_frame_num_minus4) };
            log2_max_frame_num_minus4 as u64
        });
        __bindgen_bitfield_unit.set(10usize, 2u8, {
            let pic_order_cnt_type: u32 = unsafe { ::core::mem::transmute(pic_order_cnt_type) };
            pic_order_cnt_type as u64
        });
        __bindgen_bitfield_unit.set(12usize, 4u8, {
            let log2_max_pic_order_cnt_lsb_minus4: u32 =
                unsafe { ::core::mem::transmute(log2_max_pic_order_cnt_lsb_minus4) };
            log2_max_pic_order_cnt_lsb_minus4 as u64
        });
        __bindgen_bitfield_unit.set(16usize, 1u8, {
            let delta_pic_order_always_zero_flag: u32 =
                unsafe { ::core::mem::transmute(delta_pic_order_always_zero_flag) };
            delta_pic_order_always_zero_flag as u64
        });
        __bindgen_bitfield_unit
    }
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncSequenceParameterBufferH264__bindgen_ty_1"]
        [::core::mem::size_of::<_VAEncSequenceParameterBufferH264__bindgen_ty_1>() - 4usize];
    ["Alignment of _VAEncSequenceParameterBufferH264__bindgen_ty_1"]
        [::core::mem::align_of::<_VAEncSequenceParameterBufferH264__bindgen_ty_1>() - 4usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264__bindgen_ty_1::bits"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264__bindgen_ty_1, bits) - 0usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264__bindgen_ty_1::value"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264__bindgen_ty_1, value) - 0usize];
};
#[repr(C)]
#[derive(Copy, Clone)]
pub union _VAEncSequenceParameterBufferH264__bindgen_ty_2 {
    pub bits: _VAEncSequenceParameterBufferH264__bindgen_ty_2__bindgen_ty_1,
    pub value: u32,
}
#[repr(C)]
#[repr(align(4))]
#[derive(Debug, Copy, Clone)]
pub struct _VAEncSequenceParameterBufferH264__bindgen_ty_2__bindgen_ty_1 {
    pub _bitfield_align_1: [u16; 0],
    pub _bitfield_1: __BindgenBitfieldUnit<[u8; 4usize]>,
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncSequenceParameterBufferH264__bindgen_ty_2__bindgen_ty_1"][::core::mem::size_of::<
        _VAEncSequenceParameterBufferH264__bindgen_ty_2__bindgen_ty_1,
    >() - 4usize];
    ["Alignment of _VAEncSequenceParameterBufferH264__bindgen_ty_2__bindgen_ty_1"]
        [::core::mem::align_of::<_VAEncSequenceParameterBufferH264__bindgen_ty_2__bindgen_ty_1>()
            - 4usize];
};
impl _VAEncSequenceParameterBufferH264__bindgen_ty_2__bindgen_ty_1 {
    #[inline]
    pub fn aspect_ratio_info_present_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(0usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_aspect_ratio_info_present_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(0usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn aspect_ratio_info_present_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                0usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_aspect_ratio_info_present_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                0usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn timing_info_present_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(1usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_timing_info_present_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(1usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn timing_info_present_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                1usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_timing_info_present_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                1usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn bitstream_restriction_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(2usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_bitstream_restriction_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(2usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn bitstream_restriction_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                2usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_bitstream_restriction_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                2usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn log2_max_mv_length_horizontal(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(3usize, 5u8) as u32) }
    }
    #[inline]
    pub fn set_log2_max_mv_length_horizontal(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(3usize, 5u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn log2_max_mv_length_horizontal_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                3usize,
                5u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_log2_max_mv_length_horizontal_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                3usize,
                5u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn log2_max_mv_length_vertical(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(8usize, 5u8) as u32) }
    }
    #[inline]
    pub fn set_log2_max_mv_length_vertical(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(8usize, 5u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn log2_max_mv_length_vertical_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                8usize,
                5u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_log2_max_mv_length_vertical_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                8usize,
                5u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn fixed_frame_rate_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(13usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_fixed_frame_rate_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(13usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn fixed_frame_rate_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                13usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_fixed_frame_rate_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                13usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn low_delay_hrd_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(14usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_low_delay_hrd_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(14usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn low_delay_hrd_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                14usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_low_delay_hrd_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                14usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn motion_vectors_over_pic_boundaries_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(15usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_motion_vectors_over_pic_boundaries_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(15usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn motion_vectors_over_pic_boundaries_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                15usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_motion_vectors_over_pic_boundaries_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                15usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn reserved(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(16usize, 16u8) as u32) }
    }
    #[inline]
    pub fn set_reserved(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(16usize, 16u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn reserved_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 4usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                16usize,
                16u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_reserved_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 4usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                16usize,
                16u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn new_bitfield_1(
        aspect_ratio_info_present_flag: u32,
        timing_info_present_flag: u32,
        bitstream_restriction_flag: u32,
        log2_max_mv_length_horizontal: u32,
        log2_max_mv_length_vertical: u32,
        fixed_frame_rate_flag: u32,
        low_delay_hrd_flag: u32,
        motion_vectors_over_pic_boundaries_flag: u32,
        reserved: u32,
    ) -> __BindgenBitfieldUnit<[u8; 4usize]> {
        let mut __bindgen_bitfield_unit: __BindgenBitfieldUnit<[u8; 4usize]> = Default::default();
        __bindgen_bitfield_unit.set(0usize, 1u8, {
            let aspect_ratio_info_present_flag: u32 =
                unsafe { ::core::mem::transmute(aspect_ratio_info_present_flag) };
            aspect_ratio_info_present_flag as u64
        });
        __bindgen_bitfield_unit.set(1usize, 1u8, {
            let timing_info_present_flag: u32 =
                unsafe { ::core::mem::transmute(timing_info_present_flag) };
            timing_info_present_flag as u64
        });
        __bindgen_bitfield_unit.set(2usize, 1u8, {
            let bitstream_restriction_flag: u32 =
                unsafe { ::core::mem::transmute(bitstream_restriction_flag) };
            bitstream_restriction_flag as u64
        });
        __bindgen_bitfield_unit.set(3usize, 5u8, {
            let log2_max_mv_length_horizontal: u32 =
                unsafe { ::core::mem::transmute(log2_max_mv_length_horizontal) };
            log2_max_mv_length_horizontal as u64
        });
        __bindgen_bitfield_unit.set(8usize, 5u8, {
            let log2_max_mv_length_vertical: u32 =
                unsafe { ::core::mem::transmute(log2_max_mv_length_vertical) };
            log2_max_mv_length_vertical as u64
        });
        __bindgen_bitfield_unit.set(13usize, 1u8, {
            let fixed_frame_rate_flag: u32 =
                unsafe { ::core::mem::transmute(fixed_frame_rate_flag) };
            fixed_frame_rate_flag as u64
        });
        __bindgen_bitfield_unit.set(14usize, 1u8, {
            let low_delay_hrd_flag: u32 = unsafe { ::core::mem::transmute(low_delay_hrd_flag) };
            low_delay_hrd_flag as u64
        });
        __bindgen_bitfield_unit.set(15usize, 1u8, {
            let motion_vectors_over_pic_boundaries_flag: u32 =
                unsafe { ::core::mem::transmute(motion_vectors_over_pic_boundaries_flag) };
            motion_vectors_over_pic_boundaries_flag as u64
        });
        __bindgen_bitfield_unit.set(16usize, 16u8, {
            let reserved: u32 = unsafe { ::core::mem::transmute(reserved) };
            reserved as u64
        });
        __bindgen_bitfield_unit
    }
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncSequenceParameterBufferH264__bindgen_ty_2"]
        [::core::mem::size_of::<_VAEncSequenceParameterBufferH264__bindgen_ty_2>() - 4usize];
    ["Alignment of _VAEncSequenceParameterBufferH264__bindgen_ty_2"]
        [::core::mem::align_of::<_VAEncSequenceParameterBufferH264__bindgen_ty_2>() - 4usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264__bindgen_ty_2::bits"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264__bindgen_ty_2, bits) - 0usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264__bindgen_ty_2::value"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264__bindgen_ty_2, value) - 0usize];
};
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncSequenceParameterBufferH264"]
        [::core::mem::size_of::<_VAEncSequenceParameterBufferH264>() - 1132usize];
    ["Alignment of _VAEncSequenceParameterBufferH264"]
        [::core::mem::align_of::<_VAEncSequenceParameterBufferH264>() - 4usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::seq_parameter_set_id"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264, seq_parameter_set_id) - 0usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::level_idc"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264, level_idc) - 1usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::intra_period"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264, intra_period) - 4usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::intra_idr_period"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264, intra_idr_period) - 8usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::ip_period"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264, ip_period) - 12usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::bits_per_second"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264, bits_per_second) - 16usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::max_num_ref_frames"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264, max_num_ref_frames) - 20usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::picture_width_in_mbs"][::core::mem::offset_of!(
        _VAEncSequenceParameterBufferH264,
        picture_width_in_mbs
    ) - 24usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::picture_height_in_mbs"][::core::mem::offset_of!(
        _VAEncSequenceParameterBufferH264,
        picture_height_in_mbs
    ) - 26usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::seq_fields"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264, seq_fields) - 28usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::bit_depth_luma_minus8"][::core::mem::offset_of!(
        _VAEncSequenceParameterBufferH264,
        bit_depth_luma_minus8
    ) - 32usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::bit_depth_chroma_minus8"][::core::mem::offset_of!(
        _VAEncSequenceParameterBufferH264,
        bit_depth_chroma_minus8
    ) - 33usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::num_ref_frames_in_pic_order_cnt_cycle"][::core::mem::offset_of!(
        _VAEncSequenceParameterBufferH264,
        num_ref_frames_in_pic_order_cnt_cycle
    )
        - 34usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::offset_for_non_ref_pic"][::core::mem::offset_of!(
        _VAEncSequenceParameterBufferH264,
        offset_for_non_ref_pic
    ) - 36usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::offset_for_top_to_bottom_field"][::core::mem::offset_of!(
        _VAEncSequenceParameterBufferH264,
        offset_for_top_to_bottom_field
    )
        - 40usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::offset_for_ref_frame"][::core::mem::offset_of!(
        _VAEncSequenceParameterBufferH264,
        offset_for_ref_frame
    ) - 44usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::frame_cropping_flag"][::core::mem::offset_of!(
        _VAEncSequenceParameterBufferH264,
        frame_cropping_flag
    ) - 1068usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::frame_crop_left_offset"][::core::mem::offset_of!(
        _VAEncSequenceParameterBufferH264,
        frame_crop_left_offset
    ) - 1072usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::frame_crop_right_offset"][::core::mem::offset_of!(
        _VAEncSequenceParameterBufferH264,
        frame_crop_right_offset
    ) - 1076usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::frame_crop_top_offset"][::core::mem::offset_of!(
        _VAEncSequenceParameterBufferH264,
        frame_crop_top_offset
    ) - 1080usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::frame_crop_bottom_offset"][::core::mem::offset_of!(
        _VAEncSequenceParameterBufferH264,
        frame_crop_bottom_offset
    ) - 1084usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::vui_parameters_present_flag"][::core::mem::offset_of!(
        _VAEncSequenceParameterBufferH264,
        vui_parameters_present_flag
    )
        - 1088usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::vui_fields"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264, vui_fields) - 1092usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::aspect_ratio_idc"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264, aspect_ratio_idc) - 1096usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::sar_width"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264, sar_width) - 1100usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::sar_height"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264, sar_height) - 1104usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::num_units_in_tick"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264, num_units_in_tick) - 1108usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::time_scale"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264, time_scale) - 1112usize];
    ["Offset of field: _VAEncSequenceParameterBufferH264::va_reserved"]
        [::core::mem::offset_of!(_VAEncSequenceParameterBufferH264, va_reserved) - 1116usize];
};
pub type VAEncSequenceParameterBufferH264 = _VAEncSequenceParameterBufferH264;
#[repr(C)]
#[derive(Copy, Clone)]
pub struct _VAEncPictureParameterBufferH264 {
    pub CurrPic: VAPictureH264,
    pub ReferenceFrames: [VAPictureH264; 16usize],
    pub coded_buf: VABufferID,
    pub pic_parameter_set_id: u8,
    pub seq_parameter_set_id: u8,
    pub last_picture: u8,
    pub frame_num: u16,
    pub pic_init_qp: u8,
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub chroma_qp_index_offset: i8,
    pub second_chroma_qp_index_offset: i8,
    pub pic_fields: _VAEncPictureParameterBufferH264__bindgen_ty_1,
    pub va_reserved: [u32; 4usize],
}
#[repr(C)]
#[derive(Copy, Clone)]
pub union _VAEncPictureParameterBufferH264__bindgen_ty_1 {
    pub bits: _VAEncPictureParameterBufferH264__bindgen_ty_1__bindgen_ty_1,
    pub value: u32,
}
#[repr(C)]
#[repr(align(4))]
#[derive(Debug, Copy, Clone)]
pub struct _VAEncPictureParameterBufferH264__bindgen_ty_1__bindgen_ty_1 {
    pub _bitfield_align_1: [u8; 0],
    pub _bitfield_1: __BindgenBitfieldUnit<[u8; 2usize]>,
    pub __bindgen_padding_0: u16,
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncPictureParameterBufferH264__bindgen_ty_1__bindgen_ty_1"][::core::mem::size_of::<
        _VAEncPictureParameterBufferH264__bindgen_ty_1__bindgen_ty_1,
    >() - 4usize];
    ["Alignment of _VAEncPictureParameterBufferH264__bindgen_ty_1__bindgen_ty_1"]
        [::core::mem::align_of::<_VAEncPictureParameterBufferH264__bindgen_ty_1__bindgen_ty_1>()
            - 4usize];
};
impl _VAEncPictureParameterBufferH264__bindgen_ty_1__bindgen_ty_1 {
    #[inline]
    pub fn idr_pic_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(0usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_idr_pic_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(0usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn idr_pic_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 2usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                0usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_idr_pic_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 2usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                0usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn reference_pic_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(1usize, 2u8) as u32) }
    }
    #[inline]
    pub fn set_reference_pic_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(1usize, 2u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn reference_pic_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 2usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                1usize,
                2u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_reference_pic_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 2usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                1usize,
                2u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn entropy_coding_mode_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(3usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_entropy_coding_mode_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(3usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn entropy_coding_mode_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 2usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                3usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_entropy_coding_mode_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 2usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                3usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn weighted_pred_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(4usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_weighted_pred_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(4usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn weighted_pred_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 2usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                4usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_weighted_pred_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 2usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                4usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn weighted_bipred_idc(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(5usize, 2u8) as u32) }
    }
    #[inline]
    pub fn set_weighted_bipred_idc(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(5usize, 2u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn weighted_bipred_idc_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 2usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                5usize,
                2u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_weighted_bipred_idc_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 2usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                5usize,
                2u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn constrained_intra_pred_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(7usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_constrained_intra_pred_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(7usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn constrained_intra_pred_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 2usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                7usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_constrained_intra_pred_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 2usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                7usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn transform_8x8_mode_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(8usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_transform_8x8_mode_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(8usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn transform_8x8_mode_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 2usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                8usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_transform_8x8_mode_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 2usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                8usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn deblocking_filter_control_present_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(9usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_deblocking_filter_control_present_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(9usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn deblocking_filter_control_present_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 2usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                9usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_deblocking_filter_control_present_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 2usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                9usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn redundant_pic_cnt_present_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(10usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_redundant_pic_cnt_present_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(10usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn redundant_pic_cnt_present_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 2usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                10usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_redundant_pic_cnt_present_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 2usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                10usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn pic_order_present_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(11usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_pic_order_present_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(11usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn pic_order_present_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 2usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                11usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_pic_order_present_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 2usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                11usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn pic_scaling_matrix_present_flag(&self) -> u32 {
        unsafe { ::core::mem::transmute(self._bitfield_1.get(12usize, 1u8) as u32) }
    }
    #[inline]
    pub fn set_pic_scaling_matrix_present_flag(&mut self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            self._bitfield_1.set(12usize, 1u8, val as u64)
        }
    }
    #[inline]
    pub unsafe fn pic_scaling_matrix_present_flag_raw(this: *const Self) -> u32 {
        unsafe {
            ::core::mem::transmute(<__BindgenBitfieldUnit<[u8; 2usize]>>::raw_get(
                ::core::ptr::addr_of!((*this)._bitfield_1),
                12usize,
                1u8,
            ) as u32)
        }
    }
    #[inline]
    pub unsafe fn set_pic_scaling_matrix_present_flag_raw(this: *mut Self, val: u32) {
        unsafe {
            let val: u32 = ::core::mem::transmute(val);
            <__BindgenBitfieldUnit<[u8; 2usize]>>::raw_set(
                ::core::ptr::addr_of_mut!((*this)._bitfield_1),
                12usize,
                1u8,
                val as u64,
            )
        }
    }
    #[inline]
    pub fn new_bitfield_1(
        idr_pic_flag: u32,
        reference_pic_flag: u32,
        entropy_coding_mode_flag: u32,
        weighted_pred_flag: u32,
        weighted_bipred_idc: u32,
        constrained_intra_pred_flag: u32,
        transform_8x8_mode_flag: u32,
        deblocking_filter_control_present_flag: u32,
        redundant_pic_cnt_present_flag: u32,
        pic_order_present_flag: u32,
        pic_scaling_matrix_present_flag: u32,
    ) -> __BindgenBitfieldUnit<[u8; 2usize]> {
        let mut __bindgen_bitfield_unit: __BindgenBitfieldUnit<[u8; 2usize]> = Default::default();
        __bindgen_bitfield_unit.set(0usize, 1u8, {
            let idr_pic_flag: u32 = unsafe { ::core::mem::transmute(idr_pic_flag) };
            idr_pic_flag as u64
        });
        __bindgen_bitfield_unit.set(1usize, 2u8, {
            let reference_pic_flag: u32 = unsafe { ::core::mem::transmute(reference_pic_flag) };
            reference_pic_flag as u64
        });
        __bindgen_bitfield_unit.set(3usize, 1u8, {
            let entropy_coding_mode_flag: u32 =
                unsafe { ::core::mem::transmute(entropy_coding_mode_flag) };
            entropy_coding_mode_flag as u64
        });
        __bindgen_bitfield_unit.set(4usize, 1u8, {
            let weighted_pred_flag: u32 = unsafe { ::core::mem::transmute(weighted_pred_flag) };
            weighted_pred_flag as u64
        });
        __bindgen_bitfield_unit.set(5usize, 2u8, {
            let weighted_bipred_idc: u32 = unsafe { ::core::mem::transmute(weighted_bipred_idc) };
            weighted_bipred_idc as u64
        });
        __bindgen_bitfield_unit.set(7usize, 1u8, {
            let constrained_intra_pred_flag: u32 =
                unsafe { ::core::mem::transmute(constrained_intra_pred_flag) };
            constrained_intra_pred_flag as u64
        });
        __bindgen_bitfield_unit.set(8usize, 1u8, {
            let transform_8x8_mode_flag: u32 =
                unsafe { ::core::mem::transmute(transform_8x8_mode_flag) };
            transform_8x8_mode_flag as u64
        });
        __bindgen_bitfield_unit.set(9usize, 1u8, {
            let deblocking_filter_control_present_flag: u32 =
                unsafe { ::core::mem::transmute(deblocking_filter_control_present_flag) };
            deblocking_filter_control_present_flag as u64
        });
        __bindgen_bitfield_unit.set(10usize, 1u8, {
            let redundant_pic_cnt_present_flag: u32 =
                unsafe { ::core::mem::transmute(redundant_pic_cnt_present_flag) };
            redundant_pic_cnt_present_flag as u64
        });
        __bindgen_bitfield_unit.set(11usize, 1u8, {
            let pic_order_present_flag: u32 =
                unsafe { ::core::mem::transmute(pic_order_present_flag) };
            pic_order_present_flag as u64
        });
        __bindgen_bitfield_unit.set(12usize, 1u8, {
            let pic_scaling_matrix_present_flag: u32 =
                unsafe { ::core::mem::transmute(pic_scaling_matrix_present_flag) };
            pic_scaling_matrix_present_flag as u64
        });
        __bindgen_bitfield_unit
    }
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncPictureParameterBufferH264__bindgen_ty_1"]
        [::core::mem::size_of::<_VAEncPictureParameterBufferH264__bindgen_ty_1>() - 4usize];
    ["Alignment of _VAEncPictureParameterBufferH264__bindgen_ty_1"]
        [::core::mem::align_of::<_VAEncPictureParameterBufferH264__bindgen_ty_1>() - 4usize];
    ["Offset of field: _VAEncPictureParameterBufferH264__bindgen_ty_1::bits"]
        [::core::mem::offset_of!(_VAEncPictureParameterBufferH264__bindgen_ty_1, bits) - 0usize];
    ["Offset of field: _VAEncPictureParameterBufferH264__bindgen_ty_1::value"]
        [::core::mem::offset_of!(_VAEncPictureParameterBufferH264__bindgen_ty_1, value) - 0usize];
};
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncPictureParameterBufferH264"]
        [::core::mem::size_of::<_VAEncPictureParameterBufferH264>() - 648usize];
    ["Alignment of _VAEncPictureParameterBufferH264"]
        [::core::mem::align_of::<_VAEncPictureParameterBufferH264>() - 4usize];
    ["Offset of field: _VAEncPictureParameterBufferH264::CurrPic"]
        [::core::mem::offset_of!(_VAEncPictureParameterBufferH264, CurrPic) - 0usize];
    ["Offset of field: _VAEncPictureParameterBufferH264::ReferenceFrames"]
        [::core::mem::offset_of!(_VAEncPictureParameterBufferH264, ReferenceFrames) - 36usize];
    ["Offset of field: _VAEncPictureParameterBufferH264::coded_buf"]
        [::core::mem::offset_of!(_VAEncPictureParameterBufferH264, coded_buf) - 612usize];
    ["Offset of field: _VAEncPictureParameterBufferH264::pic_parameter_set_id"][::core::mem::offset_of!(
        _VAEncPictureParameterBufferH264,
        pic_parameter_set_id
    ) - 616usize];
    ["Offset of field: _VAEncPictureParameterBufferH264::seq_parameter_set_id"][::core::mem::offset_of!(
        _VAEncPictureParameterBufferH264,
        seq_parameter_set_id
    ) - 617usize];
    ["Offset of field: _VAEncPictureParameterBufferH264::last_picture"]
        [::core::mem::offset_of!(_VAEncPictureParameterBufferH264, last_picture) - 618usize];
    ["Offset of field: _VAEncPictureParameterBufferH264::frame_num"]
        [::core::mem::offset_of!(_VAEncPictureParameterBufferH264, frame_num) - 620usize];
    ["Offset of field: _VAEncPictureParameterBufferH264::pic_init_qp"]
        [::core::mem::offset_of!(_VAEncPictureParameterBufferH264, pic_init_qp) - 622usize];
    ["Offset of field: _VAEncPictureParameterBufferH264::num_ref_idx_l0_active_minus1"][::core::mem::offset_of!(
        _VAEncPictureParameterBufferH264,
        num_ref_idx_l0_active_minus1
    )
        - 623usize];
    ["Offset of field: _VAEncPictureParameterBufferH264::num_ref_idx_l1_active_minus1"][::core::mem::offset_of!(
        _VAEncPictureParameterBufferH264,
        num_ref_idx_l1_active_minus1
    )
        - 624usize];
    ["Offset of field: _VAEncPictureParameterBufferH264::chroma_qp_index_offset"][::core::mem::offset_of!(
        _VAEncPictureParameterBufferH264,
        chroma_qp_index_offset
    ) - 625usize];
    ["Offset of field: _VAEncPictureParameterBufferH264::second_chroma_qp_index_offset"][::core::mem::offset_of!(
        _VAEncPictureParameterBufferH264,
        second_chroma_qp_index_offset
    )
        - 626usize];
    ["Offset of field: _VAEncPictureParameterBufferH264::pic_fields"]
        [::core::mem::offset_of!(_VAEncPictureParameterBufferH264, pic_fields) - 628usize];
    ["Offset of field: _VAEncPictureParameterBufferH264::va_reserved"]
        [::core::mem::offset_of!(_VAEncPictureParameterBufferH264, va_reserved) - 632usize];
};
pub type VAEncPictureParameterBufferH264 = _VAEncPictureParameterBufferH264;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _VAEncSliceParameterBufferH264 {
    pub macroblock_address: u32,
    pub num_macroblocks: u32,
    pub macroblock_info: VABufferID,
    pub slice_type: u8,
    pub pic_parameter_set_id: u8,
    pub idr_pic_id: u16,
    pub pic_order_cnt_lsb: u16,
    pub delta_pic_order_cnt_bottom: i32,
    pub delta_pic_order_cnt: [i32; 2usize],
    pub direct_spatial_mv_pred_flag: u8,
    pub num_ref_idx_active_override_flag: u8,
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub RefPicList0: [VAPictureH264; 32usize],
    pub RefPicList1: [VAPictureH264; 32usize],
    pub luma_log2_weight_denom: u8,
    pub chroma_log2_weight_denom: u8,
    pub luma_weight_l0_flag: u8,
    pub luma_weight_l0: [core::ffi::c_short; 32usize],
    pub luma_offset_l0: [core::ffi::c_short; 32usize],
    pub chroma_weight_l0_flag: u8,
    pub chroma_weight_l0: [[core::ffi::c_short; 2usize]; 32usize],
    pub chroma_offset_l0: [[core::ffi::c_short; 2usize]; 32usize],
    pub luma_weight_l1_flag: u8,
    pub luma_weight_l1: [core::ffi::c_short; 32usize],
    pub luma_offset_l1: [core::ffi::c_short; 32usize],
    pub chroma_weight_l1_flag: u8,
    pub chroma_weight_l1: [[core::ffi::c_short; 2usize]; 32usize],
    pub chroma_offset_l1: [[core::ffi::c_short; 2usize]; 32usize],
    pub cabac_init_idc: u8,
    pub slice_qp_delta: i8,
    pub disable_deblocking_filter_idc: u8,
    pub slice_alpha_c0_offset_div2: i8,
    pub slice_beta_offset_div2: i8,
    pub va_reserved: [u32; 4usize],
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAEncSliceParameterBufferH264"]
        [::core::mem::size_of::<_VAEncSliceParameterBufferH264>() - 3140usize];
    ["Alignment of _VAEncSliceParameterBufferH264"]
        [::core::mem::align_of::<_VAEncSliceParameterBufferH264>() - 4usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::macroblock_address"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, macroblock_address) - 0usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::num_macroblocks"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, num_macroblocks) - 4usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::macroblock_info"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, macroblock_info) - 8usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::slice_type"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, slice_type) - 12usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::pic_parameter_set_id"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, pic_parameter_set_id) - 13usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::idr_pic_id"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, idr_pic_id) - 14usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::pic_order_cnt_lsb"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, pic_order_cnt_lsb) - 16usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::delta_pic_order_cnt_bottom"][::core::mem::offset_of!(
        _VAEncSliceParameterBufferH264,
        delta_pic_order_cnt_bottom
    ) - 20usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::delta_pic_order_cnt"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, delta_pic_order_cnt) - 24usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::direct_spatial_mv_pred_flag"][::core::mem::offset_of!(
        _VAEncSliceParameterBufferH264,
        direct_spatial_mv_pred_flag
    ) - 32usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::num_ref_idx_active_override_flag"][::core::mem::offset_of!(
        _VAEncSliceParameterBufferH264,
        num_ref_idx_active_override_flag
    )
        - 33usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::num_ref_idx_l0_active_minus1"][::core::mem::offset_of!(
        _VAEncSliceParameterBufferH264,
        num_ref_idx_l0_active_minus1
    ) - 34usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::num_ref_idx_l1_active_minus1"][::core::mem::offset_of!(
        _VAEncSliceParameterBufferH264,
        num_ref_idx_l1_active_minus1
    ) - 35usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::RefPicList0"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, RefPicList0) - 36usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::RefPicList1"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, RefPicList1) - 1188usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::luma_log2_weight_denom"][::core::mem::offset_of!(
        _VAEncSliceParameterBufferH264,
        luma_log2_weight_denom
    ) - 2340usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::chroma_log2_weight_denom"][::core::mem::offset_of!(
        _VAEncSliceParameterBufferH264,
        chroma_log2_weight_denom
    ) - 2341usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::luma_weight_l0_flag"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, luma_weight_l0_flag) - 2342usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::luma_weight_l0"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, luma_weight_l0) - 2344usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::luma_offset_l0"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, luma_offset_l0) - 2408usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::chroma_weight_l0_flag"][::core::mem::offset_of!(
        _VAEncSliceParameterBufferH264,
        chroma_weight_l0_flag
    ) - 2472usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::chroma_weight_l0"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, chroma_weight_l0) - 2474usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::chroma_offset_l0"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, chroma_offset_l0) - 2602usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::luma_weight_l1_flag"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, luma_weight_l1_flag) - 2730usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::luma_weight_l1"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, luma_weight_l1) - 2732usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::luma_offset_l1"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, luma_offset_l1) - 2796usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::chroma_weight_l1_flag"][::core::mem::offset_of!(
        _VAEncSliceParameterBufferH264,
        chroma_weight_l1_flag
    ) - 2860usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::chroma_weight_l1"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, chroma_weight_l1) - 2862usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::chroma_offset_l1"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, chroma_offset_l1) - 2990usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::cabac_init_idc"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, cabac_init_idc) - 3118usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::slice_qp_delta"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, slice_qp_delta) - 3119usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::disable_deblocking_filter_idc"][::core::mem::offset_of!(
        _VAEncSliceParameterBufferH264,
        disable_deblocking_filter_idc
    )
        - 3120usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::slice_alpha_c0_offset_div2"][::core::mem::offset_of!(
        _VAEncSliceParameterBufferH264,
        slice_alpha_c0_offset_div2
    ) - 3121usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::slice_beta_offset_div2"][::core::mem::offset_of!(
        _VAEncSliceParameterBufferH264,
        slice_beta_offset_div2
    ) - 3122usize];
    ["Offset of field: _VAEncSliceParameterBufferH264::va_reserved"]
        [::core::mem::offset_of!(_VAEncSliceParameterBufferH264, va_reserved) - 3124usize];
};
pub type VAEncSliceParameterBufferH264 = _VAEncSliceParameterBufferH264;
pub const _VAProcColorStandardType_VAProcColorStandardNone: _VAProcColorStandardType = 0;
pub const _VAProcColorStandardType_VAProcColorStandardBT601: _VAProcColorStandardType = 1;
pub const _VAProcColorStandardType_VAProcColorStandardBT709: _VAProcColorStandardType = 2;
pub const _VAProcColorStandardType_VAProcColorStandardBT470M: _VAProcColorStandardType = 3;
pub const _VAProcColorStandardType_VAProcColorStandardBT470BG: _VAProcColorStandardType = 4;
pub const _VAProcColorStandardType_VAProcColorStandardSMPTE170M: _VAProcColorStandardType = 5;
pub const _VAProcColorStandardType_VAProcColorStandardSMPTE240M: _VAProcColorStandardType = 6;
pub const _VAProcColorStandardType_VAProcColorStandardGenericFilm: _VAProcColorStandardType = 7;
pub const _VAProcColorStandardType_VAProcColorStandardSRGB: _VAProcColorStandardType = 8;
pub const _VAProcColorStandardType_VAProcColorStandardSTRGB: _VAProcColorStandardType = 9;
pub const _VAProcColorStandardType_VAProcColorStandardXVYCC601: _VAProcColorStandardType = 10;
pub const _VAProcColorStandardType_VAProcColorStandardXVYCC709: _VAProcColorStandardType = 11;
pub const _VAProcColorStandardType_VAProcColorStandardBT2020: _VAProcColorStandardType = 12;
pub const _VAProcColorStandardType_VAProcColorStandardExplicit: _VAProcColorStandardType = 13;
pub const _VAProcColorStandardType_VAProcColorStandardCount: _VAProcColorStandardType = 14;
pub type _VAProcColorStandardType = core::ffi::c_uint;
pub use self::_VAProcColorStandardType as VAProcColorStandardType;
pub const _VAProcHighDynamicRangeMetadataType_VAProcHighDynamicRangeMetadataNone:
    _VAProcHighDynamicRangeMetadataType = 0;
pub const _VAProcHighDynamicRangeMetadataType_VAProcHighDynamicRangeMetadataHDR10:
    _VAProcHighDynamicRangeMetadataType = 1;
pub const _VAProcHighDynamicRangeMetadataType_VAProcHighDynamicRangeMetadataTypeCount:
    _VAProcHighDynamicRangeMetadataType = 2;
pub type _VAProcHighDynamicRangeMetadataType = core::ffi::c_uint;
pub use self::_VAProcHighDynamicRangeMetadataType as VAProcHighDynamicRangeMetadataType;
pub const _VAProcMode_VAProcDefaultMode: _VAProcMode = 0;
pub const _VAProcMode_VAProcPowerSavingMode: _VAProcMode = 1;
pub const _VAProcMode_VAProcPerformanceMode: _VAProcMode = 2;
pub type _VAProcMode = core::ffi::c_uint;
pub use self::_VAProcMode as VAProcMode;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _VABlendState {
    pub flags: core::ffi::c_uint,
    pub global_alpha: f32,
    pub min_luma: f32,
    pub max_luma: f32,
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VABlendState"][::core::mem::size_of::<_VABlendState>() - 16usize];
    ["Alignment of _VABlendState"][::core::mem::align_of::<_VABlendState>() - 4usize];
    ["Offset of field: _VABlendState::flags"]
        [::core::mem::offset_of!(_VABlendState, flags) - 0usize];
    ["Offset of field: _VABlendState::global_alpha"]
        [::core::mem::offset_of!(_VABlendState, global_alpha) - 4usize];
    ["Offset of field: _VABlendState::min_luma"]
        [::core::mem::offset_of!(_VABlendState, min_luma) - 8usize];
    ["Offset of field: _VABlendState::max_luma"]
        [::core::mem::offset_of!(_VABlendState, max_luma) - 12usize];
};
pub type VABlendState = _VABlendState;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _VAProcColorProperties {
    pub chroma_sample_location: u8,
    pub color_range: u8,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub reserved: [u8; 3usize],
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAProcColorProperties"][::core::mem::size_of::<_VAProcColorProperties>() - 8usize];
    ["Alignment of _VAProcColorProperties"]
        [::core::mem::align_of::<_VAProcColorProperties>() - 1usize];
    ["Offset of field: _VAProcColorProperties::chroma_sample_location"]
        [::core::mem::offset_of!(_VAProcColorProperties, chroma_sample_location) - 0usize];
    ["Offset of field: _VAProcColorProperties::color_range"]
        [::core::mem::offset_of!(_VAProcColorProperties, color_range) - 1usize];
    ["Offset of field: _VAProcColorProperties::colour_primaries"]
        [::core::mem::offset_of!(_VAProcColorProperties, colour_primaries) - 2usize];
    ["Offset of field: _VAProcColorProperties::transfer_characteristics"]
        [::core::mem::offset_of!(_VAProcColorProperties, transfer_characteristics) - 3usize];
    ["Offset of field: _VAProcColorProperties::matrix_coefficients"]
        [::core::mem::offset_of!(_VAProcColorProperties, matrix_coefficients) - 4usize];
    ["Offset of field: _VAProcColorProperties::reserved"]
        [::core::mem::offset_of!(_VAProcColorProperties, reserved) - 5usize];
};
pub type VAProcColorProperties = _VAProcColorProperties;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _VAHdrMetaData {
    pub metadata_type: VAProcHighDynamicRangeMetadataType,
    pub metadata: *mut core::ffi::c_void,
    pub metadata_size: u32,
    pub reserved: [u32; 4usize],
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAHdrMetaData"][::core::mem::size_of::<_VAHdrMetaData>() - 40usize];
    ["Alignment of _VAHdrMetaData"][::core::mem::align_of::<_VAHdrMetaData>() - 8usize];
    ["Offset of field: _VAHdrMetaData::metadata_type"]
        [::core::mem::offset_of!(_VAHdrMetaData, metadata_type) - 0usize];
    ["Offset of field: _VAHdrMetaData::metadata"]
        [::core::mem::offset_of!(_VAHdrMetaData, metadata) - 8usize];
    ["Offset of field: _VAHdrMetaData::metadata_size"]
        [::core::mem::offset_of!(_VAHdrMetaData, metadata_size) - 16usize];
    ["Offset of field: _VAHdrMetaData::reserved"]
        [::core::mem::offset_of!(_VAHdrMetaData, reserved) - 20usize];
};
pub type VAHdrMetaData = _VAHdrMetaData;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _VAProcPipelineParameterBuffer {
    pub surface: VASurfaceID,
    pub surface_region: *const VARectangle,
    pub surface_color_standard: VAProcColorStandardType,
    pub output_region: *const VARectangle,
    pub output_background_color: u32,
    pub output_color_standard: VAProcColorStandardType,
    pub pipeline_flags: u32,
    pub filter_flags: u32,
    pub filters: *mut VABufferID,
    pub num_filters: u32,
    pub forward_references: *mut VASurfaceID,
    pub num_forward_references: u32,
    pub backward_references: *mut VASurfaceID,
    pub num_backward_references: u32,
    pub rotation_state: u32,
    pub blend_state: *const VABlendState,
    pub mirror_state: u32,
    pub additional_outputs: *mut VASurfaceID,
    pub num_additional_outputs: u32,
    pub input_surface_flag: u32,
    pub output_surface_flag: u32,
    pub input_color_properties: VAProcColorProperties,
    pub output_color_properties: VAProcColorProperties,
    pub processing_mode: VAProcMode,
    pub output_hdr_metadata: *mut VAHdrMetaData,
    pub va_reserved: [u32; 16usize],
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VAProcPipelineParameterBuffer"]
        [::core::mem::size_of::<_VAProcPipelineParameterBuffer>() - 224usize];
    ["Alignment of _VAProcPipelineParameterBuffer"]
        [::core::mem::align_of::<_VAProcPipelineParameterBuffer>() - 8usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::surface"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, surface) - 0usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::surface_region"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, surface_region) - 8usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::surface_color_standard"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, surface_color_standard) - 16usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::output_region"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, output_region) - 24usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::output_background_color"][::core::mem::offset_of!(
        _VAProcPipelineParameterBuffer,
        output_background_color
    ) - 32usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::output_color_standard"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, output_color_standard) - 36usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::pipeline_flags"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, pipeline_flags) - 40usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::filter_flags"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, filter_flags) - 44usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::filters"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, filters) - 48usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::num_filters"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, num_filters) - 56usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::forward_references"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, forward_references) - 64usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::num_forward_references"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, num_forward_references) - 72usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::backward_references"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, backward_references) - 80usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::num_backward_references"][::core::mem::offset_of!(
        _VAProcPipelineParameterBuffer,
        num_backward_references
    ) - 88usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::rotation_state"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, rotation_state) - 92usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::blend_state"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, blend_state) - 96usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::mirror_state"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, mirror_state) - 104usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::additional_outputs"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, additional_outputs) - 112usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::num_additional_outputs"][::core::mem::offset_of!(
        _VAProcPipelineParameterBuffer,
        num_additional_outputs
    ) - 120usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::input_surface_flag"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, input_surface_flag) - 124usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::output_surface_flag"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, output_surface_flag) - 128usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::input_color_properties"][::core::mem::offset_of!(
        _VAProcPipelineParameterBuffer,
        input_color_properties
    ) - 132usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::output_color_properties"][::core::mem::offset_of!(
        _VAProcPipelineParameterBuffer,
        output_color_properties
    ) - 140usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::processing_mode"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, processing_mode) - 148usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::output_hdr_metadata"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, output_hdr_metadata) - 152usize];
    ["Offset of field: _VAProcPipelineParameterBuffer::va_reserved"]
        [::core::mem::offset_of!(_VAProcPipelineParameterBuffer, va_reserved) - 160usize];
};
pub type VAProcPipelineParameterBuffer = _VAProcPipelineParameterBuffer;
pub const VA_DRM_AUTH_NONE: _bindgen_ty_1 = 0;
pub const VA_DRM_AUTH_DRI1: _bindgen_ty_1 = 1;
pub const VA_DRM_AUTH_DRI2: _bindgen_ty_1 = 2;
pub const VA_DRM_AUTH_CUSTOM: _bindgen_ty_1 = 3;
pub type _bindgen_ty_1 = core::ffi::c_uint;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _VADRMPRIMESurfaceDescriptor {
    pub fourcc: u32,
    pub width: u32,
    pub height: u32,
    pub num_objects: u32,
    pub objects: [_VADRMPRIMESurfaceDescriptor__bindgen_ty_1; 4usize],
    pub num_layers: u32,
    pub layers: [_VADRMPRIMESurfaceDescriptor__bindgen_ty_2; 4usize],
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _VADRMPRIMESurfaceDescriptor__bindgen_ty_1 {
    pub fd: core::ffi::c_int,
    pub size: u32,
    pub drm_format_modifier: u64,
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VADRMPRIMESurfaceDescriptor__bindgen_ty_1"]
        [::core::mem::size_of::<_VADRMPRIMESurfaceDescriptor__bindgen_ty_1>() - 16usize];
    ["Alignment of _VADRMPRIMESurfaceDescriptor__bindgen_ty_1"]
        [::core::mem::align_of::<_VADRMPRIMESurfaceDescriptor__bindgen_ty_1>() - 8usize];
    ["Offset of field: _VADRMPRIMESurfaceDescriptor__bindgen_ty_1::fd"]
        [::core::mem::offset_of!(_VADRMPRIMESurfaceDescriptor__bindgen_ty_1, fd) - 0usize];
    ["Offset of field: _VADRMPRIMESurfaceDescriptor__bindgen_ty_1::size"]
        [::core::mem::offset_of!(_VADRMPRIMESurfaceDescriptor__bindgen_ty_1, size) - 4usize];
    ["Offset of field: _VADRMPRIMESurfaceDescriptor__bindgen_ty_1::drm_format_modifier"][::core::mem::offset_of!(
        _VADRMPRIMESurfaceDescriptor__bindgen_ty_1,
        drm_format_modifier
    )
        - 8usize];
};
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _VADRMPRIMESurfaceDescriptor__bindgen_ty_2 {
    pub drm_format: u32,
    pub num_planes: u32,
    pub object_index: [u32; 4usize],
    pub offset: [u32; 4usize],
    pub pitch: [u32; 4usize],
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VADRMPRIMESurfaceDescriptor__bindgen_ty_2"]
        [::core::mem::size_of::<_VADRMPRIMESurfaceDescriptor__bindgen_ty_2>() - 56usize];
    ["Alignment of _VADRMPRIMESurfaceDescriptor__bindgen_ty_2"]
        [::core::mem::align_of::<_VADRMPRIMESurfaceDescriptor__bindgen_ty_2>() - 4usize];
    ["Offset of field: _VADRMPRIMESurfaceDescriptor__bindgen_ty_2::drm_format"]
        [::core::mem::offset_of!(_VADRMPRIMESurfaceDescriptor__bindgen_ty_2, drm_format) - 0usize];
    ["Offset of field: _VADRMPRIMESurfaceDescriptor__bindgen_ty_2::num_planes"]
        [::core::mem::offset_of!(_VADRMPRIMESurfaceDescriptor__bindgen_ty_2, num_planes) - 4usize];
    ["Offset of field: _VADRMPRIMESurfaceDescriptor__bindgen_ty_2::object_index"][::core::mem::offset_of!(
        _VADRMPRIMESurfaceDescriptor__bindgen_ty_2,
        object_index
    ) - 8usize];
    ["Offset of field: _VADRMPRIMESurfaceDescriptor__bindgen_ty_2::offset"]
        [::core::mem::offset_of!(_VADRMPRIMESurfaceDescriptor__bindgen_ty_2, offset) - 24usize];
    ["Offset of field: _VADRMPRIMESurfaceDescriptor__bindgen_ty_2::pitch"]
        [::core::mem::offset_of!(_VADRMPRIMESurfaceDescriptor__bindgen_ty_2, pitch) - 40usize];
};
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of _VADRMPRIMESurfaceDescriptor"]
        [::core::mem::size_of::<_VADRMPRIMESurfaceDescriptor>() - 312usize];
    ["Alignment of _VADRMPRIMESurfaceDescriptor"]
        [::core::mem::align_of::<_VADRMPRIMESurfaceDescriptor>() - 8usize];
    ["Offset of field: _VADRMPRIMESurfaceDescriptor::fourcc"]
        [::core::mem::offset_of!(_VADRMPRIMESurfaceDescriptor, fourcc) - 0usize];
    ["Offset of field: _VADRMPRIMESurfaceDescriptor::width"]
        [::core::mem::offset_of!(_VADRMPRIMESurfaceDescriptor, width) - 4usize];
    ["Offset of field: _VADRMPRIMESurfaceDescriptor::height"]
        [::core::mem::offset_of!(_VADRMPRIMESurfaceDescriptor, height) - 8usize];
    ["Offset of field: _VADRMPRIMESurfaceDescriptor::num_objects"]
        [::core::mem::offset_of!(_VADRMPRIMESurfaceDescriptor, num_objects) - 12usize];
    ["Offset of field: _VADRMPRIMESurfaceDescriptor::objects"]
        [::core::mem::offset_of!(_VADRMPRIMESurfaceDescriptor, objects) - 16usize];
    ["Offset of field: _VADRMPRIMESurfaceDescriptor::num_layers"]
        [::core::mem::offset_of!(_VADRMPRIMESurfaceDescriptor, num_layers) - 80usize];
    ["Offset of field: _VADRMPRIMESurfaceDescriptor::layers"]
        [::core::mem::offset_of!(_VADRMPRIMESurfaceDescriptor, layers) - 84usize];
};
pub type VADRMPRIMESurfaceDescriptor = _VADRMPRIMESurfaceDescriptor;
