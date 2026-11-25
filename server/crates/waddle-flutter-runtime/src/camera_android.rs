//! JNI entry points for the local Android WebRTC capture thread.

use std::ptr;

use jni::errors::{Result, ThrowRuntimeExAndDefault};
use jni::objects::{JByteArray, JByteBuffer, JClass};
use jni::sys::{jboolean, jint, jlong};
use jni::{Env, EnvUnowned};

use crate::camera_abi::{
    waddle_camera_effect_create, waddle_camera_effect_destroy, waddle_camera_effect_process,
    WaddleCameraEffect, WaddleCameraInputPlane, WaddleCameraOutputPlane,
};

#[unsafe(no_mangle)]
extern "system" fn Java_social_waddle_flutter_CameraEffects_create(
    mut caller: EnvUnowned,
    _class: JClass,
    image: JByteArray,
    width: jint,
    height: jint,
) -> jlong {
    caller
        .with_env(|env| -> Result<_> {
            let rgba = if image.is_null() {
                None
            } else {
                Some(env.convert_byte_array(&image)?)
            };
            let (data, length) = rgba
                .as_ref()
                .map_or((ptr::null(), 0), |v| (v.as_ptr(), v.len()));
            // SAFETY: The model copies this bounded image before returning.
            Ok(
                unsafe { waddle_camera_effect_create(data, length, width as u32, height as u32) }
                    as jlong,
            )
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

fn input(env: &Env, plane: &JByteBuffer, stride: jint) -> Result<WaddleCameraInputPlane> {
    Ok(WaddleCameraInputPlane {
        data: env.get_direct_buffer_address(plane)?,
        length: env.get_direct_buffer_capacity(plane)?,
        stride: stride as usize,
    })
}

fn output(env: &Env, plane: &JByteBuffer, stride: jint) -> Result<WaddleCameraOutputPlane> {
    Ok(WaddleCameraOutputPlane {
        data: env.get_direct_buffer_address(plane)?,
        length: env.get_direct_buffer_capacity(plane)?,
        stride: stride as usize,
    })
}

#[unsafe(no_mangle)]
extern "system" fn Java_social_waddle_flutter_CameraEffects_process(
    mut caller: EnvUnowned,
    _class: JClass,
    handle: jlong,
    width: jint,
    height: jint,
    rotation: jint,
    y: JByteBuffer,
    sy: jint,
    u: JByteBuffer,
    su: jint,
    v: JByteBuffer,
    sv: jint,
    dy: JByteBuffer,
    dsy: jint,
    du: JByteBuffer,
    dsu: jint,
    dv: JByteBuffer,
    dsv: jint,
) -> jboolean {
    caller
        .with_env(|env| -> Result<_> {
            let source = [
                input(env, &y, sy)?,
                input(env, &u, su)?,
                input(env, &v, sv)?,
            ];
            let dest = [
                output(env, &dy, dsy)?,
                output(env, &du, dsu)?,
                output(env, &dv, dsv)?,
            ];
            // SAFETY: The synchronous WebRTC callback retains its input. JavaI420Buffer
            // owns the disjoint output planes, and removal waits for this callback.
            Ok(unsafe {
                waddle_camera_effect_process(
                    handle as *mut WaddleCameraEffect,
                    width as u32,
                    height as u32,
                    rotation as u32,
                    source.as_ptr(),
                    dest.as_ptr(),
                )
            })
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
extern "system" fn Java_social_waddle_flutter_CameraEffects_destroy(
    _env: EnvUnowned,
    _class: JClass,
    handle: jlong,
) {
    // SAFETY: Java removes the processor under WebRTC's callback lock first
    // and releases its exclusively owned model exactly once.
    unsafe { waddle_camera_effect_destroy(handle as *mut WaddleCameraEffect) }
}
