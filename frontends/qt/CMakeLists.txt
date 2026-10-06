qt_standard_project_setup()

qt_add_executable(tern
    src/main.cpp
    src/core.cpp
    src/foldermodel.cpp
    src/messagelistmodel.cpp
    src/messageview.cpp
    src/sourcewindow.cpp
    src/composewindow.cpp
    src/mainwindow.cpp
    src/profilepicker.cpp
)

# The app icon, embedded for the window icon (works without installing).
qt_add_resources(tern "icons"
    PREFIX "/"
    FILES icons/fr.karlsen.Tern.svg)

target_include_directories(tern PRIVATE
    "${TERN_FFI_INCLUDE_DIR}"           # tern-ffi/src/lib.rs.h, rust/cxx.h
    "${PROJECT_SOURCE_DIR}/crates"      # tern-ffi/include/sink.h
)

target_link_libraries(tern PRIVATE
    tern_ffi
    Qt6::DBus
    Qt6::Widgets
    Qt6::WebEngineCore
    Qt6::WebEngineWidgets
)

# cargo (re)generates the bridge headers during the build. Declaring them as
# by-products of a step that runs after cargo tells Ninja they can change
# mid-build, so every source including them is recompiled. Without this, files
# that didn't change themselves kept the old struct layouts (and the linker
# could pick a stale inline copy constructor).
add_custom_target(tern_ffi_headers
    COMMAND "${CMAKE_COMMAND}" -E true
    BYPRODUCTS
        "${TERN_FFI_INCLUDE_DIR}/tern-ffi/src/lib.rs.h"
        "${TERN_FFI_INCLUDE_DIR}/rust/cxx.h"
    COMMENT "Checking generated cxx headers"
    VERBATIM)
add_dependencies(tern_ffi_headers cargo-build_tern_ffi)
add_dependencies(tern tern_ffi_headers)

target_compile_definitions(tern PRIVATE
    QT_NO_KEYWORDS
    QT_NO_CAST_FROM_ASCII
    TERN_VERSION="${PROJECT_VERSION}"
)
if(CMAKE_CXX_COMPILER_ID MATCHES "GNU|Clang")
    target_compile_options(tern PRIVATE -Wall -Wextra -Wpedantic)
endif()

include(GNUInstallDirs)
install(TARGETS tern RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR})
install(FILES fr.karlsen.Tern.desktop DESTINATION ${CMAKE_INSTALL_DATADIR}/applications)
install(FILES icons/fr.karlsen.Tern.svg DESTINATION ${CMAKE_INSTALL_DATADIR}/icons/hicolor/scalable/apps)
