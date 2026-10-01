//! Typed dataref handles.

use std::ffi::c_void;
use std::marker::PhantomData;

use crate::sys;
use crate::util::to_cstring;

/// A value type that can be read and written as a whole dataref.
pub trait Scalar: Copy + Default {
    #[doc(hidden)]
    unsafe fn get(raw: sys::XPLMDataRef) -> Self;
    #[doc(hidden)]
    unsafe fn set(raw: sys::XPLMDataRef, value: Self);
}

impl Scalar for i32 {
    unsafe fn get(raw: sys::XPLMDataRef) -> Self {
        unsafe { sys::XPLMGetDatai(raw) }
    }
    unsafe fn set(raw: sys::XPLMDataRef, value: Self) {
        unsafe { sys::XPLMSetDatai(raw, value) }
    }
}

impl Scalar for f32 {
    unsafe fn get(raw: sys::XPLMDataRef) -> Self {
        unsafe { sys::XPLMGetDataf(raw) }
    }
    unsafe fn set(raw: sys::XPLMDataRef, value: Self) {
        unsafe { sys::XPLMSetDataf(raw, value) }
    }
}

impl Scalar for f64 {
    unsafe fn get(raw: sys::XPLMDataRef) -> Self {
        unsafe { sys::XPLMGetDatad(raw) }
    }
    unsafe fn set(raw: sys::XPLMDataRef, value: Self) {
        unsafe { sys::XPLMSetDatad(raw, value) }
    }
}

/// An element type of an array dataref.
pub trait Element: Copy + Default {
    /// Element count; XPLM returns it when the output pointer is null.
    #[doc(hidden)]
    unsafe fn count(raw: sys::XPLMDataRef) -> i32;
    #[doc(hidden)]
    unsafe fn get(raw: sys::XPLMDataRef, out: &mut [Self], offset: i32) -> i32;
    #[doc(hidden)]
    unsafe fn set(raw: sys::XPLMDataRef, values: &[Self], offset: i32);
}

impl Element for f32 {
    unsafe fn count(raw: sys::XPLMDataRef) -> i32 {
        unsafe { sys::XPLMGetDatavf(raw, std::ptr::null_mut(), 0, 0) }
    }
    unsafe fn get(raw: sys::XPLMDataRef, out: &mut [Self], offset: i32) -> i32 {
        unsafe { sys::XPLMGetDatavf(raw, out.as_mut_ptr(), offset, len_i32(out.len())) }
    }
    unsafe fn set(raw: sys::XPLMDataRef, values: &[Self], offset: i32) {
        // XPLM takes a mutable pointer but does not write through it.
        unsafe {
            sys::XPLMSetDatavf(
                raw,
                values.as_ptr() as *mut f32,
                offset,
                len_i32(values.len()),
            )
        }
    }
}

impl Element for i32 {
    unsafe fn count(raw: sys::XPLMDataRef) -> i32 {
        unsafe { sys::XPLMGetDatavi(raw, std::ptr::null_mut(), 0, 0) }
    }
    unsafe fn get(raw: sys::XPLMDataRef, out: &mut [Self], offset: i32) -> i32 {
        unsafe { sys::XPLMGetDatavi(raw, out.as_mut_ptr(), offset, len_i32(out.len())) }
    }
    unsafe fn set(raw: sys::XPLMDataRef, values: &[Self], offset: i32) {
        unsafe {
            sys::XPLMSetDatavi(
                raw,
                values.as_ptr() as *mut i32,
                offset,
                len_i32(values.len()),
            )
        }
    }
}

impl Element for u8 {
    unsafe fn count(raw: sys::XPLMDataRef) -> i32 {
        unsafe { sys::XPLMGetDatab(raw, std::ptr::null_mut(), 0, 0) }
    }
    unsafe fn get(raw: sys::XPLMDataRef, out: &mut [Self], offset: i32) -> i32 {
        unsafe {
            sys::XPLMGetDatab(
                raw,
                out.as_mut_ptr() as *mut c_void,
                offset,
                len_i32(out.len()),
            )
        }
    }
    unsafe fn set(raw: sys::XPLMDataRef, values: &[Self], offset: i32) {
        unsafe {
            sys::XPLMSetDatab(
                raw,
                values.as_ptr() as *mut c_void,
                offset,
                len_i32(values.len()),
            )
        }
    }
}

fn len_i32(len: usize) -> i32 {
    i32::try_from(len).unwrap_or(i32::MAX)
}

/// Looks up a dataref, returning `None` if X-Plane (or the plugin that owns
/// it) does not publish it.
fn find_raw(name: &str) -> Option<sys::XPLMDataRef> {
    let name = to_cstring(name);
    let raw = unsafe { sys::XPLMFindDataRef(name.as_ptr()) };
    (!raw.is_null()).then_some(raw)
}

/// A scalar dataref of type `T`. Main thread only.
#[derive(Debug, Clone, Copy)]
pub struct DataRef<T: Scalar> {
    raw: sys::XPLMDataRef,
    _type: PhantomData<(T, *const ())>,
}

impl<T: Scalar> DataRef<T> {
    pub fn find(name: &str) -> Option<Self> {
        find_raw(name).map(Self::from_raw)
    }

    pub(crate) fn from_raw(raw: sys::XPLMDataRef) -> Self {
        Self {
            raw,
            _type: PhantomData,
        }
    }

    pub fn get(&self) -> T {
        unsafe { T::get(self.raw) }
    }

    pub fn set(&self, value: T) {
        unsafe { T::set(self.raw, value) }
    }

    pub fn is_writable(&self) -> bool {
        unsafe { sys::XPLMCanWriteDataRef(self.raw) != 0 }
    }
}

/// An array dataref with elements of type `T`. Main thread only.
#[derive(Debug, Clone, Copy)]
pub struct ArrayRef<T: Element> {
    raw: sys::XPLMDataRef,
    _type: PhantomData<(T, *const ())>,
}

impl<T: Element> ArrayRef<T> {
    pub fn find(name: &str) -> Option<Self> {
        find_raw(name).map(Self::from_raw)
    }

    pub(crate) fn from_raw(raw: sys::XPLMDataRef) -> Self {
        Self {
            raw,
            _type: PhantomData,
        }
    }

    /// Reads up to `out.len()` elements starting at `offset`; returns how many
    /// were read.
    pub fn get(&self, offset: usize, out: &mut [T]) -> usize {
        let n = unsafe { T::get(self.raw, out, len_i32(offset)) };
        n.max(0) as usize
    }

    /// Reads a single element.
    pub fn get_one(&self, index: usize) -> T {
        let mut value = [T::default()];
        self.get(index, &mut value);
        value[0]
    }

    /// Writes `values` starting at `offset`.
    pub fn set(&self, offset: usize, values: &[T]) {
        unsafe { T::set(self.raw, values, len_i32(offset)) }
    }

    /// Writes a single element.
    pub fn set_one(&self, index: usize, value: T) {
        self.set(index, &[value]);
    }

    /// Number of elements the dataref currently holds.
    pub fn len(&self) -> usize {
        let n = unsafe { T::count(self.raw) };
        n.max(0) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_writable(&self) -> bool {
        unsafe { sys::XPLMCanWriteDataRef(self.raw) != 0 }
    }
}
