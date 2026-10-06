# Top-level build: the Rust core (through Corrosion) and the Qt frontend.
#
#   cmake -B build -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo
#   cmake --build build
#
# `cargo build` / `cargo test` keep working on their own for the Rust crates.
cmake_minimum_required(VERSION 3.24)
project(tern VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 23)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

if(CMAKE_CXX_COMPILER_ID STREQUAL "GNU" AND CMAKE_CXX_COMPILER_VERSION VERSION_LESS 14)
    message(FATAL_ERROR "GCC 14 or newer is required (found ${CMAKE_CXX_COMPILER_VERSION})")
elseif(CMAKE_CXX_COMPILER_ID MATCHES "Clang" AND CMAKE_CXX_COMPILER_VERSION VERSION_LESS 18)
    message(FATAL_ERROR "Clang 18 or newer is required (found ${CMAKE_CXX_COMPILER_VERSION})")
endif()

find_package(Qt6 6.8 REQUIRED COMPONENTS DBus Widgets WebEngineCore WebEngineWidgets)
# freedesktop sound theme playback (new-mail sound).
find_package(PkgConfig REQUIRED)
pkg_check_modules(CANBERRA REQUIRED IMPORTED_TARGET libcanberra)

# Prefer a system Corrosion (distribution builds are offline), fall back to
# fetching it.
find_package(Corrosion 0.5 QUIET)
if(NOT Corrosion_FOUND)
    include(FetchContent)
    FetchContent_Declare(Corrosion
        GIT_REPOSITORY https://github.com/corrosion-rs/corrosion.git
        GIT_TAG v0.6.1)
    FetchContent_MakeAvailable(Corrosion)
endif()

corrosion_import_crate(MANIFEST_PATH Cargo.toml CRATES tern-ffi CRATE_TYPES staticlib)

# build.rs copies the generated cxx headers here.
set(TERN_FFI_INCLUDE_DIR "${CMAKE_BINARY_DIR}/cxxbridge")
corrosion_set_env_vars(tern_ffi
    "TERN_FFI_INCLUDE_DIR=${TERN_FFI_INCLUDE_DIR}"
    "CXX=${CMAKE_CXX_COMPILER}")

add_subdirectory(frontends/qt)
