cmake_minimum_required(VERSION 3.18)
project(raptor_flutter_jni LANGUAGES C)

add_library(raptor_flutter_jni SHARED
    src/main/jni/raptor_flutter_jni.c
)

# Link Android NDK libraries
target_link_libraries(raptor_flutter_jni
    android    # ANativeWindow_fromSurface
    log        # Android logging
)
