//! The JNI entry point: `memo.app.Native.call(request: ByteArray, data: ByteArray?): ByteArray`.
//! Only byte arrays cross, so strings never go through JNI's modified UTF-8.

use jni_sys::{JNIEnv, jbyte, jbyteArray, jclass, jsize};

#[unsafe(no_mangle)]
pub extern "system" fn Java_memo_app_Native_call(
    env: *mut JNIEnv,
    _class: jclass,
    request: jbyteArray,
    data: jbyteArray,
) -> jbyteArray {
    // SAFETY: the JVM passes a valid env and local references (or null for `data`).
    unsafe {
        let Some(request) = read(env, request) else { return std::ptr::null_mut() };
        let data = read(env, data);
        let reply = crate::call(&request, data.as_deref());
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
        let len = (functions.GetArrayLength.expect("JNI"))(env, array);
        let mut bytes = vec![0u8; len.max(0) as usize];
        (functions.GetByteArrayRegion.expect("JNI"))(env, array, 0, len, bytes.as_mut_ptr().cast::<jbyte>());
        Some(bytes)
    }
}

/// Null if the JVM is out of memory; it then has an `OutOfMemoryError` pending.
unsafe fn write(env: *mut JNIEnv, bytes: &[u8]) -> jbyteArray {
    unsafe {
        let functions = &**env;
        let len = bytes.len() as jsize;
        let array = (functions.NewByteArray.expect("JNI"))(env, len);
        if !array.is_null() {
            (functions.SetByteArrayRegion.expect("JNI"))(env, array, 0, len, bytes.as_ptr().cast::<jbyte>());
        }
        array
    }
}
