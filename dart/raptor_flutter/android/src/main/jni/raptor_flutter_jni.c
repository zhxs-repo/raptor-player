/**
 * raptor_flutter_jni.c — JNI bridge for ANativeWindow* extraction
 *
 * Provides Java_com_dylplayer_raptor_flutter_RaptorPlatformSurfaceView_getNativeWindowFromSurface
 * which calls Android NDK's ANativeWindow_fromSurface() to get the native
 * window pointer from a Java Surface object.
 *
 * The returned pointer (as jlong) is passed through MethodChannel to Dart,
 * then through FFI to Rust's raptor_set_surface().
 */

#include <jni.h>
#include <android/native_window_jni.h>
#include <stdint.h>

JNIEXPORT jlong JNICALL
Java_dev_dylplayer_raptor_1flutter_RaptorPlatformSurfaceView_getNativeWindowFromSurface(
    JNIEnv *env,
    jclass clazz,
    jobject surface
) {
    if (surface == NULL) {
        return 0;
    }

    ANativeWindow *window = ANativeWindow_fromSurface(env, surface);
    if (window == NULL) {
        return 0;
    }

    // ANativeWindow_fromSurface acquires a reference.
    // The Rust side's ExternalRenderer takes ownership;
    // we release our reference when the window is no longer needed.
    return (jlong)(intptr_t)window;
}
