// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <winerror.h>

namespace Microsoft::Terminal::Protocol
{
    // Private page-to-server signal, emitted only after elevate-shim starts.
    // The public COM method returns a typed JSON outcome, not this HRESULT.
    inline constexpr HRESULT ElevationHandoff = MAKE_HRESULT(SEVERITY_ERROR, FACILITY_ITF, 0xa123);
}
