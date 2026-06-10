cmake_minimum_required(VERSION 3.24)

project(CatHubFrequencyProbeNative LANGUAGES CXX)

add_executable(CatHubFrequencyProbeNative WIN32
    main.cpp
)

target_compile_features(CatHubFrequencyProbeNative PRIVATE cxx_std_20)
target_include_directories(CatHubFrequencyProbeNative PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/../../src/rust/qsoripper-ffi
)
target_compile_definitions(CatHubFrequencyProbeNative PRIVATE
    UNICODE
    _UNICODE
    WIN32_LEAN_AND_MEAN
    NOMINMAX
)
target_link_libraries(CatHubFrequencyProbeNative PRIVATE
    gdi32
    user32
    ws2_32
)

if (MSVC)
    target_compile_options(CatHubFrequencyProbeNative PRIVATE /W4 /permissive-)
endif()
