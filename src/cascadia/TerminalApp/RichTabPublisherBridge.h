// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <windows.h>

extern "C" __declspec(dllimport) HRESULT RichTabPublishSnapshot(
    BSTR lease,
    BSTR snapshotJson,
    BOOL* succeeded,
    BSTR* message) noexcept;
