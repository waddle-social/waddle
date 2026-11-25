//! Native local-camera hook ABI. One capture callback owns and serializes each handle.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::{ptr, slice};

use waddle_camera_effects::{contains_plane, CameraEffect, Geometry, Plane, PlaneMut};

#[repr(C)]
pub struct WaddleCameraEffect(CameraEffect);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WaddleCameraInputPlane {
    pub data: *const u8,
    pub length: usize,
    pub stride: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WaddleCameraOutputPlane {
    pub data: *mut u8,
    pub length: usize,
    pub stride: usize,
}

/// Creates a camera-local effect. Pass null and zero sizes for blur.
///
/// # Safety
/// Non-null RGBA storage must be readable for the specified length until this call returns.
#[no_mangle]
pub unsafe extern "C" fn waddle_camera_effect_create(
    rgba: *const u8,
    length: usize,
    width: u32,
    height: u32,
) -> *mut WaddleCameraEffect {
    catch_unwind(|| {
        let (w, h) = (width as usize, height as usize);
        let image = if rgba.is_null() && (length, w, h) == (0, 0, 0) {
            None
        } else if !rgba.is_null()
            && (1..=1024).contains(&w)
            && (1..=1024).contains(&h)
            && length == 4 * w * h
        {
            // SAFETY: the caller supplies a readable image and its dimensions bound the slice.
            Some((unsafe { slice::from_raw_parts(rgba, length) }, w, h))
        } else {
            return ptr::null_mut();
        };
        CameraEffect::new(image).map_or(ptr::null_mut(), |effect| {
            Box::into_raw(Box::new(WaddleCameraEffect(effect)))
        })
    })
    .unwrap_or(ptr::null_mut())
}

/// Writes an I420 frame in original camera orientation; false means suppress the output.
///
/// # Safety
/// The caller exclusively owns the live handle and three writable, disjoint output planes.
/// The three input planes are readable, disjoint from the outputs, and all storage remains
/// valid until this call returns. Both descriptor arrays contain three entries.
#[no_mangle]
pub unsafe extern "C" fn waddle_camera_effect_process(
    effect: *mut WaddleCameraEffect,
    width: u32,
    height: u32,
    rotation: u32,
    input: *const WaddleCameraInputPlane,
    output: *const WaddleCameraOutputPlane,
) -> bool {
    if effect.is_null() || input.is_null() || output.is_null() {
        return false;
    }
    let Some(g) = Geometry::new(width as usize, height as usize, rotation) else {
        return false;
    };
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the C caller supplies three valid descriptors per array.
        let (src, dst) = unsafe {
            (
                *(input.cast::<[WaddleCameraInputPlane; 3]>()),
                *(output.cast::<[WaddleCameraOutputPlane; 3]>()),
            )
        };
        for (ch, (i, o)) in src.iter().zip(&dst).enumerate() {
            let (w, h) = g.plane_size(ch);
            if i.data.is_null()
                || o.data.is_null()
                || !contains_plane(i.length, i.stride, w, h)
                || !contains_plane(o.length, o.stride, w, h)
                || i.length > isize::MAX as usize
                || o.length > isize::MAX as usize
            {
                return false;
            }
        }
        let input = src.map(|i| Plane {
            // SAFETY: the validated length fits its caller-owned input buffer.
            data: unsafe { slice::from_raw_parts(i.data, i.length) },
            stride: i.stride,
        });
        let mut output = dst.map(|o| PlaneMut {
            // SAFETY: the validated output buffer is exclusively borrowed for this callback.
            data: unsafe { slice::from_raw_parts_mut(o.data, o.length) },
            stride: o.stride,
        });
        // SAFETY: callers stop callbacks before dropping this exclusively borrowed handle.
        unsafe { &mut *effect }.0.process(g, &input, &mut output)
    }))
    .unwrap_or(false)
}

/// Releases the model worker and any account-local background image.
///
/// # Safety
/// Pass null or an owned handle returned by create, once all capture callbacks have finished.
#[no_mangle]
pub unsafe extern "C" fn waddle_camera_effect_destroy(effect: *mut WaddleCameraEffect) {
    if !effect.is_null() {
        // SAFETY: this consumes the only ownership of the original boxed handle.
        drop(unsafe { Box::from_raw(effect) });
    }
}
