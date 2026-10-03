//! raptor-ffmpeg build script
//!
//! For PC platforms (Linux/macOS/Windows), FFmpeg is found via `ffmpeg-sys-next`'s
//! built-in pkg-config / vcpkg detection. No action needed here.
//!
//! For Android (`aarch64-linux-android`), FFmpeg must be cross-compiled separately
//! using the NDK toolchain. This build script sets up the search path based on
//! environment variables:
//!
//! - `FFMPEG_PKG_CONFIG_PATH_{arch}` — arch-specific pkg-config path
//!   (e.g. `FFMPEG_PKG_CONFIG_PATH_aarch64_linux_android=/path/to/ffmpeg/arm64/lib/pkgconfig`)
//! - `FFMPEG_DIR_{arch}` — arch-specific FFmpeg root directory (fallback)
//!   (e.g. `FFMPEG_DIR_aarch64_linux_android=/path/to/ffmpeg/arm64`)
//! - `FFMPEG_PKG_CONFIG_PATH` — generic pkg-config path
//! - `FFMPEG_DIR` — generic FFmpeg root directory (fallback)

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();

    // Only need special handling for Android
    if !target.contains("android") {
        return;
    }

    // Normalize target triple for env var lookup
    // (Rust converts target triple to uppercase + underscores for env vars)
    let target_env = target.replace('-', "_").to_uppercase();

    // Try arch-specific FFMPEG_PKG_CONFIG_PATH first, then generic
    let pkg_config_key = format!("FFMPEG_PKG_CONFIG_PATH_{target_env}");
    if let Ok(path) = std::env::var(&pkg_config_key) {
        std::env::set_var("FFMPEG_PKG_CONFIG_PATH", &path);
        println!("cargo:warning=raptor-ffmpeg: Android pkg-config path = {path}");
    } else if let Ok(path) = std::env::var("FFMPEG_PKG_CONFIG_PATH") {
        println!("cargo:warning=raptor-ffmpeg: pkg-config path = {path}");
    }

    // Try arch-specific FFMPEG_DIR first, then generic
    let dir_key = format!("FFMPEG_DIR_{target_env}");
    if let Ok(dir) = std::env::var(&dir_key) {
        std::env::set_var("FFMPEG_DIR", &dir);
        println!("cargo:warning=raptor-ffmpeg: Android FFMPEG_DIR = {dir}");

        let lib_dir = format!("{dir}/lib");
        let include_dir = format!("{dir}/include");

        // Tell the linker where to find FFmpeg .so files
        println!("cargo:rustc-link-search=native={lib_dir}");

        // Link FFmpeg libraries (order matters for static linking)
        for lib in &[
            "avformat",
            "avcodec",
            "avfilter",
            "swscale",
            "swresample",
            "avutil",
        ] {
            println!("cargo:rustc-link-lib={lib}");
        }

        // Pass include path to ffmpeg-sys-next (for bindgen)
        println!("cargo:include={include_dir}");

        // Re-run if env vars change
        println!("cargo:rerun-if-env-changed={pkg_config_key}");
        println!("cargo:rerun-if-env-changed={dir_key}");
    } else if let Ok(dir) = std::env::var("FFMPEG_DIR") {
        println!("cargo:warning=raptor-ffmpeg: FFMPEG_DIR = {dir}");
        println!("cargo:rustc-link-search=native={dir}/lib");
    } else {
        println!(
            "cargo:warning=raptor-ffmpeg: WARNING — No FFMPEG_DIR set for Android target. \
             Set FFMPEG_DIR_{target_env} or FFMPEG_DIR to the FFmpeg Android build root."
        );
    }

    println!("cargo:rerun-if-env-changed=FFMPEG_PKG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
}
