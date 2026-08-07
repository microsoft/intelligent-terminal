// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <windows.h>
#include <wincred.h>

#include <string>
#include <string_view>

#include <wil/resource.h>
#include <wil/result.h>
#include <winrt/base.h>

namespace Microsoft::Terminal::CustomModels
{
    inline constexpr std::wstring_view CredentialResource{ L"IntelligentTerminal.LocalModelProvider" };

    inline std::wstring CredentialTarget(const winrt::hstring& credentialId)
    {
        std::wstring target{ CredentialResource };
        target.push_back(L'/');
        target.append(credentialId);
        return target;
    }

    inline void RemoveApiKey(const winrt::hstring& credentialId);

    inline winrt::hstring StoreApiKey(const winrt::hstring& previousCredentialId, const winrt::hstring& apiKey)
    {
        GUID id{};
        THROW_IF_FAILED(CoCreateGuid(&id));
        const auto credentialId = winrt::to_hstring(id);

        const auto target = CredentialTarget(credentialId);
        auto apiKeyUtf8 = winrt::to_string(apiKey);
        const auto clearApiKey = wil::scope_exit([&]() noexcept {
            SecureZeroMemory(apiKeyUtf8.data(), apiKeyUtf8.size());
        });
        THROW_HR_IF(E_INVALIDARG, apiKeyUtf8.size() > CRED_MAX_CREDENTIAL_BLOB_SIZE);

        CREDENTIALW credential{};
        credential.Type = CRED_TYPE_GENERIC;
        credential.TargetName = const_cast<wchar_t*>(target.c_str());
        credential.CredentialBlobSize = static_cast<DWORD>(apiKeyUtf8.size());
        credential.CredentialBlob = reinterpret_cast<LPBYTE>(apiKeyUtf8.data());
        credential.Persist = CRED_PERSIST_LOCAL_MACHINE;
        credential.UserName = const_cast<wchar_t*>(L"Intelligent Terminal");
        THROW_IF_WIN32_BOOL_FALSE(CredWriteW(&credential, 0));

        if (!previousCredentialId.empty())
        {
            try
            {
                RemoveApiKey(previousCredentialId);
            }
            catch (...)
            {
                LOG_IF_WIN32_BOOL_FALSE(CredDeleteW(target.c_str(), CRED_TYPE_GENERIC, 0));
                throw;
            }
        }

        return credentialId;
    }

    inline void RemoveApiKey(const winrt::hstring& credentialId)
    {
        if (credentialId.empty())
        {
            return;
        }

        const auto target = CredentialTarget(credentialId);
        if (!CredDeleteW(target.c_str(), CRED_TYPE_GENERIC, 0))
        {
            const auto error = GetLastError();
            if (error != ERROR_NOT_FOUND)
            {
                THROW_WIN32(error);
            }
        }
    }
}
