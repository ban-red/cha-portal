//! A shared library opened with `dlopen` and kept for the life of the process
//! (x264, SVT-AV1 and libva are optional: the streamer starts without them).

use std::ffi::{CStr, CString, c_void};

use anyhow::{Result, bail};

pub struct Library {
    handle: *mut c_void,
    name: &'static str,
}

// SAFETY: a dlopen handle may be used from any thread.
unsafe impl Send for Library {}
unsafe impl Sync for Library {}

impl Library {
    /// The first of `names` that loads.
    pub fn open(names: &[&'static str]) -> Result<Self> {
        let mut errors = Vec::new();
        for &name in names {
            let c_name = CString::new(name).expect("library names have no NUL");
            // SAFETY: a valid C string; RTLD_NOW resolves everything up front.
            let handle =
                unsafe { libc::dlopen(c_name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if !handle.is_null() {
                return Ok(Self { handle, name });
            }
            errors.push(last_error());
        }
        bail!(
            "couldn't load {} ({})",
            names.join(" or "),
            errors.join("; ")
        )
    }

    /// The name that loaded.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The symbol `name` as a function pointer of type `T`, if there is one.
    ///
    /// # Safety
    /// `T` must be the symbol's real (pointer-sized) type.
    pub unsafe fn symbol<T: Copy>(&self, name: &CStr) -> Result<T> {
        assert_eq!(size_of::<T>(), size_of::<*mut c_void>());
        // SAFETY: a live handle and a valid C string.
        let ptr = unsafe { libc::dlsym(self.handle, name.as_ptr()) };
        if ptr.is_null() {
            bail!("{} has no {}", self.name, name.to_string_lossy());
        }
        // SAFETY: the caller vouches for `T`; sizes match (asserted above).
        Ok(unsafe { std::mem::transmute_copy(&ptr) })
    }
}

fn last_error() -> String {
    // SAFETY: dlerror returns null or a valid C string owned by libc.
    let err = unsafe { libc::dlerror() };
    if err.is_null() {
        "unknown error".into()
    } else {
        // SAFETY: checked non-null above.
        unsafe { CStr::from_ptr(err) }
            .to_string_lossy()
            .into_owned()
    }
}
