/**
 * raptor_flutter_jni.c — JNI bridge for ANativeWindow* extraction
 *
 * Provides Java_com_dylplayer_raptor_flutter_RaptorPlatformSurfaceView_getNativeWindowFromSurface
 * which calls Android NDK's ANativeWindow_fromSurface() to get the native
 * window pointer from a Java Surface object.
 *
 * The returned pointer (as jlong) is passed through MethodChannel to Dart,
 * then through FFI to Rust's raptor_set_surface().
 *
 * Ownership: ANativeWindow_fromSurface returns a +1 reference. The normal path
 * hands that reference to Rust, where wgpu takes ownership and releases it when
 * the Surface is dropped (see ExternalRenderer::detach_surface). That only
 * happens if Dart actually forwards the pointer to raptor_set_surface() --
 * if the transfer is abandoned (disposed client, RaptorException, surface torn
 * down between the request and the reply) the reference would leak, so
 * releaseNativeWindow() exists for Dart to give it back.
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

    return (jlong)(intptr_t)window;
}

/**
 * Give back a reference obtained from getNativeWindowFromSurface.
 *
 * Needed when Dart obtains a pointer but never hands it to
 * raptor_set_surface() -- otherwise the +1 reference leaks for the lifetime of
 * the process and the SurfaceView can fail to acquire a new surface.
 */
JNIEXPORT void JNICALL
Java_dev_dylplayer_raptor_1flutter_RaptorPlatformSurfaceView_releaseNativeWindow(
    JNIEnv *env,
    jclass clazz,
    jlong window
) {
    if (window == 0) {
        return;
    }
    ANativeWindow_release((ANativeWindow *)(intptr_t)window);
}
