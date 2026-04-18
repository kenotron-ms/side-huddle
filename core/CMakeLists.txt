cmake_minimum_required(VERSION 3.20)
    project(meetinglistener VERSION 0.1.0 LANGUAGES C OBJC)

    set(CMAKE_C_STANDARD 11)
    set(CMAKE_OBJC_STANDARD 11)

    # ── Source files ──────────────────────────────────────────────────────────────

    set(SOURCES
        src/monitor.c
        src/mix.c
    )

    if(APPLE)
        list(APPEND SOURCES
            src/darwin/detect.c
            src/darwin/platform.c
            src/darwin/window.m
            src/darwin/process.c
            src/darwin/record_tap.m
            src/darwin/record_mic.m
        )
    elseif(WIN32)
        list(APPEND SOURCES
            src/windows/detect.c
            src/windows/record.c
        )
    elseif(UNIX)
        list(APPEND SOURCES
            src/linux/detect.c
            src/linux/record.c
        )
    endif()

    # ── Library target ────────────────────────────────────────────────────────────

    add_library(meetinglistener SHARED ${SOURCES})

    target_include_directories(meetinglistener PUBLIC include)

    set_target_properties(meetinglistener PROPERTIES
        PUBLIC_HEADER include/meetinglistener.h
        VERSION       ${PROJECT_VERSION}
        SOVERSION     0
    )

    # ── Platform frameworks ───────────────────────────────────────────────────────

    if(APPLE)
        find_library(COREAUDIO    CoreAudio    REQUIRED)
        find_library(COREGRAPHICS CoreGraphics REQUIRED)
        find_library(COREFOUNDATION CoreFoundation REQUIRED)
        find_library(AVFOUNDATION AVFoundation REQUIRED)
        find_library(AUDIOTOOLBOX AudioToolbox REQUIRED)
        find_library(FOUNDATION   Foundation   REQUIRED)
        target_link_libraries(meetinglistener PRIVATE
            ${COREAUDIO} ${COREGRAPHICS} ${COREFOUNDATION}
            ${AVFOUNDATION} ${AUDIOTOOLBOX} ${FOUNDATION}
        )
        # ARC for ObjC files
        set_source_files_properties(
            src/darwin/record_tap.m
            src/darwin/record_mic.m
            src/darwin/window.m
            PROPERTIES COMPILE_FLAGS "-fobjc-arc"
        )
    elseif(WIN32)
        target_link_libraries(meetinglistener PRIVATE ole32 oleaut32)
    elseif(UNIX)
        find_package(PkgConfig REQUIRED)
        pkg_check_modules(PULSE libpulse)
        if(PULSE_FOUND)
            target_link_libraries(meetinglistener PRIVATE ${PULSE_LIBRARIES})
            target_include_directories(meetinglistener PRIVATE ${PULSE_INCLUDE_DIRS})
        endif()
    endif()

    # ── Install ───────────────────────────────────────────────────────────────────

    include(GNUInstallDirs)
    install(TARGETS meetinglistener
        LIBRARY  DESTINATION ${CMAKE_INSTALL_LIBDIR}
        ARCHIVE  DESTINATION ${CMAKE_INSTALL_LIBDIR}
        PUBLIC_HEADER DESTINATION ${CMAKE_INSTALL_INCLUDEDIR}
    )
    