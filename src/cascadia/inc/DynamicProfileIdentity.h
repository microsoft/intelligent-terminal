// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <guiddef.h>

// !!! LOAD-BEARING
// Changing this namespace disconnects dynamic profiles from saved settings.
// UUID-v5 names use UTF-16LE bytes, without a terminating NUL.
inline constexpr GUID TERMINAL_PROFILE_NAMESPACE_GUID = { 0x2bde4a90, 0xd05f, 0x401c, { 0x94, 0x92, 0xe4, 0x8, 0x84, 0xea, 0xd1, 0xd8 } };
