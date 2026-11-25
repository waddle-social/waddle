//! Synchronous access to the Android audio device module's direct PCM buffer.

use jni::errors::{Result, ThrowRuntimeExAndDefault};
use jni::objects::{JByteBuffer, JClass};
use jni::sys::{jboolean, jint, jlong};
use jni::EnvUnowned;

use crate::audio_abi::{
    waddle_audio_filter_create, waddle_audio_filter_destroy, waddle_audio_filter_process,
    waddle_audio_filter_reset, WaddleAudioFilter,
};

#[unsafe(no_mangle)]
extern "system" fn Java_com_cloudwebrtc_webrtc_WaddleAudioCapture_create(
    _env: EnvUnowned,
    _class: JClass,
    mode: jint,
    rate: jint,
    channels: jint,
) -> jlong {
    waddle_audio_filter_create(mode as u32, rate as u32, channels as u32) as jlong
}

#[unsafe(no_mangle)]
extern "system" fn Java_com_cloudwebrtc_webrtc_WaddleAudioCapture_process(
    mut caller: EnvUnowned,
    _class: JClass,
    handle: jlong,
    buffer: JByteBuffer,
    bytes: jint,
) -> jboolean {
    caller
        .with_env(|env| -> Result<_> {
            let address = env.get_direct_buffer_address(&buffer)?.cast::<i16>();
            let capacity = env.get_direct_buffer_capacity(&buffer)?;
            if bytes <= 0 || bytes % 2 != 0 || bytes as usize > capacity || !address.is_aligned() {
                return Ok(false);
            }
            // SAFETY: The ADM exclusively owns this buffer until its callback returns.
            // The Java capture monitor serializes processing, state resets and retirement.
            Ok(unsafe {
                waddle_audio_filter_process(
                    handle as *mut WaddleAudioFilter,
                    address,
                    bytes as usize / 2,
                )
            })
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
extern "system" fn Java_com_cloudwebrtc_webrtc_WaddleAudioCapture_reset(
    _env: EnvUnowned,
    _class: JClass,
    handle: jlong,
) {
    // SAFETY: The filter is attached and its capture monitor excludes process and destroy.
    unsafe { waddle_audio_filter_reset(handle as *mut WaddleAudioFilter) }
}

#[unsafe(no_mangle)]
extern "system" fn Java_com_cloudwebrtc_webrtc_WaddleAudioCapture_destroy(
    _env: EnvUnowned,
    _class: JClass,
    handle: jlong,
) {
    // SAFETY: The sole Java owner detaches the handle before freeing it.
    unsafe { waddle_audio_filter_destroy(handle as *mut WaddleAudioFilter) }
}
