# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Bertrand Presles
#
# Dolphin (KDE Frameworks 6) plugins of PDrive for Linux:
#  - pdriveoverlay: emblems for the sync state of the files (KOverlayIconPlugin);
#  - pdriveactions: "Keep Available Offline" / "Free Up Space" in the context menu
#    (KAbstractFileItemActionPlugin).
# Both only talk to the pdrived daemon over D-Bus (io.github.bpresles.PDrive1).
cmake_minimum_required(VERSION 3.16)
project(pdrive-dolphin VERSION 0.3.0 LANGUAGES CXX)

find_package(ECM 6.0.0 REQUIRED NO_MODULE)
set(CMAKE_MODULE_PATH ${ECM_MODULE_PATH})

include(KDEInstallDirs6)
include(KDECMakeSettings)
include(KDECompilerSettings NO_POLICY_SCOPE)
include(FeatureSummary)

find_package(Qt6 REQUIRED COMPONENTS Core DBus Widgets)
find_package(KF6 REQUIRED COMPONENTS CoreAddons I18n KIO)

set(CMAKE_AUTOMOC ON)

# Shared by both plugins, linked into each of them.
add_library(pdriveclient OBJECT pdriveclient.cpp pdriveclient.h)
set_property(TARGET pdriveclient PROPERTY POSITION_INDEPENDENT_CODE ON)
target_link_libraries(pdriveclient PUBLIC Qt6::Core Qt6::DBus)

kcoreaddons_add_plugin(pdriveoverlay INSTALL_NAMESPACE "kf6/overlayicon")
target_sources(pdriveoverlay PRIVATE pdriveoverlay.cpp pdriveoverlay.h $<TARGET_OBJECTS:pdriveclient>)
target_link_libraries(pdriveoverlay PRIVATE Qt6::DBus KF6::CoreAddons KF6::KIOCore)

kcoreaddons_add_plugin(pdriveactions INSTALL_NAMESPACE "kf6/kfileitemaction")
target_sources(pdriveactions PRIVATE pdriveactions.cpp pdriveactions.h $<TARGET_OBJECTS:pdriveclient>)
target_link_libraries(pdriveactions PRIVATE Qt6::DBus Qt6::Widgets KF6::CoreAddons KF6::I18n KF6::KIOWidgets)

feature_summary(WHAT ALL INCLUDE_QUIET_PACKAGES FATAL_ON_MISSING_REQUIRED_PACKAGES)
