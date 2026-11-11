//! `memo-native` for the Android app, over JNI:
//! `memo.app.Native.call(request: ByteArray, data: ByteArray?): ByteArray`.
//! Only byte arrays cross, so strings never go through JNI's modified UTF-8.

use jni_sys::{JNIEnv, jbyte, jbyteArray, jclass, jsize};

/// # Safety
/// Only the JVM calls this, with a valid env and local references (or null for `data`).
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_memo_app_Native_call(
    env: *mut JNIEnv,
    _class: jclass,
    request: jbyteArray,
    data: jbyteArray,
) -> jbyteArray {
    unsafe {
        let Some(request) = read(env, request) else { return std::ptr::null_mut() };
        let data = read(env, data);
        let reply = memo_native::call(&request, data.as_deref());
        write(env, &reply)
    }
}

/// `None` for a null array.
unsafe fn read(env: *mut JNIEnv, array: jbyteArray) -> Option<Vec<u8>> {
    if array.is_null() {
        return None;
    }
    unsafe {
        let functions = &**env;
        let len = (functions.v1_1.GetArrayLength)(env, array);
        let mut bytes = vec![0u8; len.max(0) as usize];
        (functions.v1_1.GetByteArrayRegion)(env, array, 0, len, bytes.as_mut_ptr().cast::<jbyte>());
        Some(bytes)
    }
}

/// Null if the JVM is out of memory; it then has an `OutOfMemoryError` pending.
unsafe fn write(env: *mut JNIEnv, bytes: &[u8]) -> jbyteArray {
    unsafe {
        let functions = &**env;
        let len = bytes.len() as jsize;
        let array = (functions.v1_1.NewByteArray)(env, len);
        if !array.is_null() {
            (functions.v1_1.SetByteArrayRegion)(env, array, 0, len, bytes.as_ptr().cast::<jbyte>());
        }
        array
    }
}
