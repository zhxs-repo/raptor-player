package dev.dylplayer.raptor_flutter

import android.content.Context
import android.view.SurfaceHolder
import android.view.SurfaceView
import io.flutter.embedding.engine.plugins.FlutterPlugin
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel
import io.flutter.plugin.common.MethodChannel.MethodCallHandler
import io.flutter.plugin.common.MethodChannel.Result
import io.flutter.plugin.common.StandardMessageCodec
import io.flutter.plugin.platform.PlatformView
import io.flutter.plugin.platform.PlatformViewFactory

/**
 * Raptor Flutter Plugin — Android SurfaceView 管理
 *
 * 注册 PlatformViewFactory 创建 SurfaceView，通过 MethodChannel 将
 * ANativeWindow* 指针传给 Dart 层，供 Rust FFI 的 raptor_set_surface 使用。
 */
class RaptorFlutterPlugin : FlutterPlugin, MethodCallHandler {

    private lateinit var channel: MethodChannel
    private val surfaceViews = mutableMapOf<Int, RaptorPlatformSurfaceView>()

    override fun onAttachedToEngine(binding: FlutterPlugin.FlutterPluginBinding) {
        channel = MethodChannel(binding.binaryMessenger, CHANNEL_NAME)
        channel.setMethodCallHandler(this)

        binding.platformViewRegistry.registerViewFactory(
            VIEW_TYPE,
            RaptorSurfaceViewFactory(this)
        )
    }

    override fun onDetachedFromEngine(binding: FlutterPlugin.FlutterPluginBinding) {
        channel.setMethodCallHandler(null)
        surfaceViews.clear()
    }

    override fun onMethodCall(call: MethodCall, result: Result) {
        when (call.method) {
            "getNativeWindow" -> {
                val viewId = call.argument<Int>("viewId")
                if (viewId == null) {
                    result.error("INVALID_ARGUMENT", "viewId is required", null)
                    return
                }
                val surfaceView = surfaceViews[viewId]
                if (surfaceView == null || !surfaceView.isSurfaceValid) {
                    result.error("SURFACE_NOT_READY", "Surface not available for view $viewId", null)
                    return
                }
                val nativeWindow = surfaceView.getNativeWindowPointer()
                val width = surfaceView.viewWidth
                val height = surfaceView.viewHeight
                result.success(mapOf(
                    "nativeWindow" to nativeWindow,
                    "width" to width,
                    "height" to height,
                ))
            }
            "destroyView" -> {
                val viewId = call.argument<Int>("viewId")
                if (viewId != null) {
                    surfaceViews.remove(viewId)
                }
                result.success(null)
            }
            else -> result.notImplemented()
        }
    }

    internal fun registerSurfaceView(viewId: Int, view: RaptorPlatformSurfaceView) {
        surfaceViews[viewId] = view
    }

    internal fun unregisterSurfaceView(viewId: Int) {
        surfaceViews.remove(viewId)
    }

    companion object {
        const val CHANNEL_NAME = "dev.dylplayer.raptor_flutter"
        const val VIEW_TYPE = "dev.dylplayer.raptor_flutter/surface_view"
    }
}

/**
 * PlatformViewFactory — 创建 SurfaceView PlatformView 实例
 */
class RaptorSurfaceViewFactory(
    private val plugin: RaptorFlutterPlugin
) : PlatformViewFactory(StandardMessageCodec.INSTANCE) {

    override fun create(context: Context, viewId: Int, args: Any?): PlatformView {
        val creationParams = args as? Map<*, *>
        val surfaceView = RaptorPlatformSurfaceView(context, viewId, plugin)
        plugin.registerSurfaceView(viewId, surfaceView)
        return surfaceView
    }
}

/**
 * PlatformView 包装器 — 管理 SurfaceView 和 SurfaceHolder 回调
 *
 * 通过 JNI `ANativeWindow_fromSurface` 获取原生窗口指针。
 * 注意：此指针在 Surface 有效期间有效，Surface 销毁后失效。
 */
class RaptorPlatformSurfaceView(
    context: Context,
    private val viewId: Int,
    private val plugin: RaptorFlutterPlugin
) : PlatformView, SurfaceHolder.Callback {

    private val surfaceView = SurfaceView(context)
    var isSurfaceValid = false
        private set
    var viewWidth = 0
        private set
    var viewHeight = 0
        private set

    private var surfaceHolder: SurfaceHolder? = null

    init {
        surfaceView.holder.addCallback(this)
    }

    override fun getView(): SurfaceView = surfaceView

    override fun dispose() {
        surfaceView.holder.removeCallback(this)
        plugin.unregisterSurfaceView(viewId)
    }

    /**
     * 获取 ANativeWindow* 指针（作为 long 值）
     *
     * 使用 JNI 调用 ANativeWindow_fromSurface(env, surface) 获取。
     * 此值在 Surface 有效期间有效。
     */
    fun getNativeWindowPointer(): Long {
        val holder = surfaceHolder ?: return 0L
        return getNativeWindowFromSurface(holder.surface)
    }

    // ── SurfaceHolder.Callback ───────────────────────────────────

    override fun surfaceCreated(holder: SurfaceHolder) {
        surfaceHolder = holder
        isSurfaceValid = true
        viewWidth = holder.surfaceFrame.width()
        viewHeight = holder.surfaceFrame.height()
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        surfaceHolder = holder
        viewWidth = width
        viewHeight = height
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        isSurfaceValid = false
        surfaceHolder = null
    }

    // ── JNI ──────────────────────────────────────────────────────

    companion object {
        init {
            try {
                System.loadLibrary("raptor_flutter_jni")
            } catch (_: UnsatisfiedLinkError) {
                // JNI library not available — getNativeWindowFromSurface will return 0
            }
        }

        /**
         * JNI: 调用 ANativeWindow_fromSurface() 获取原生窗口指针。
         *
         * 如果 JNI 库未加载，返回 0。
         */
        @JvmStatic
        private external fun getNativeWindowFromSurface(surface: android.view.Surface): Long
    }
}
