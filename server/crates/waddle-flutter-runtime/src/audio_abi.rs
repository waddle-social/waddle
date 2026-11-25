//! Per-capture PCM16 processing for the native microphone adapters.
use std::panic::{catch_unwind, AssertUnwindSafe};
use waddle_audio_filters::{AudioFilter, Mode};

pub struct WaddleAudioFilter(AudioFilter);

#[unsafe(no_mangle)]
pub extern "C" fn waddle_audio_filter_create(
    mode: u32,
    sample_rate: u32,
    channels: u32,
) -> *mut WaddleAudioFilter {
    catch_unwind(|| {
        let filter = AudioFilter::new(Mode::from_id(mode)?, sample_rate, channels)?;
        Some(Box::into_raw(Box::new(WaddleAudioFilter(filter))))
    })
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// # Safety
/// The handle must be live and exclusive, and samples must reference count writable aligned i16 values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn waddle_audio_filter_process(
    filter: *mut WaddleAudioFilter,
    samples: *mut i16,
    count: usize,
) -> bool {
    // SAFETY: The caller grants exclusive access to the live handle for this callback.
    let Some(filter) = (unsafe { filter.as_mut() }) else {
        return false;
    };
    if samples.is_null() || !samples.is_aligned() || !filter.0.accepts(count) {
        return false;
    }
    // SAFETY: Geometry and alignment are checked above. The caller owns the writable region.
    let pcm = unsafe { std::slice::from_raw_parts_mut(samples, count) };
    catch_unwind(AssertUnwindSafe(|| filter.0.process(pcm))).unwrap_or(false)
}
/// # Safety
/// The handle must be live and exclusive.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn waddle_audio_filter_reset(filter: *mut WaddleAudioFilter) {
    // SAFETY: The capture adapter excludes concurrent callbacks during reset.
    if let Some(filter) = unsafe { filter.as_mut() } {
        let _ = catch_unwind(AssertUnwindSafe(|| filter.0.reset()));
    }
}
/// # Safety
/// The handle must be uniquely owned and detached from all capture callbacks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn waddle_audio_filter_destroy(filter: *mut WaddleAudioFilter) {
    if !filter.is_null() {
        // SAFETY: The detached handle came from Box::into_raw and is consumed exactly once.
        drop(unsafe { Box::from_raw(filter) });
    }
}

#[cfg(test)]
mod tests {
    use super::{
        waddle_audio_filter_create, waddle_audio_filter_destroy, waddle_audio_filter_process,
        waddle_audio_filter_reset,
    };

    #[test]
    fn invalid_buffers_are_rejected_before_dereferencing_memory() {
        assert!(waddle_audio_filter_create(3, 48_000, 1).is_null());
        let handle = waddle_audio_filter_create(0, 48_000, 2);
        assert!(!handle.is_null());
        let mut input = [3i16, -16];
        // SAFETY: The sole handle is live through reset and is dropped once. Invalid
        // inputs exercise the checked C boundary; none describes a valid writable slice.
        unsafe {
            assert!(!waddle_audio_filter_process(
                std::ptr::null_mut(),
                input.as_mut_ptr(),
                2
            ));
            assert!(!waddle_audio_filter_process(
                handle,
                std::ptr::null_mut(),
                2
            ));
            assert!(!waddle_audio_filter_process(handle, input.as_mut_ptr(), 1));
            assert!(!waddle_audio_filter_process(
                handle,
                input.as_mut_ptr(),
                usize::MAX
            ));
            let misaligned = input.as_mut_ptr().cast::<u8>().add(1).cast::<i16>();
            assert!(!waddle_audio_filter_process(handle, misaligned, 2));
            assert!(waddle_audio_filter_process(handle, input.as_mut_ptr(), 2));
            assert_eq!(input, [3, -16]);
            waddle_audio_filter_reset(handle);
            waddle_audio_filter_destroy(handle);
        }
    }
}
