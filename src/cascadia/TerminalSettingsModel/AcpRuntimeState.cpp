// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "AcpRuntimeState.h"

#include "AcpRuntimeState.g.cpp"
#include "AcpModelInfo.g.cpp"

namespace winrt::Microsoft::Terminal::Settings::Model::implementation
{
    namespace
    {
        // The single instance lives inside this DLL's globals; both
        // TerminalApp and TerminalSettingsEditor reach it via
        // AcpRuntimeState::Current(), routed through the WinRT activation
        // factory exported from Microsoft.Terminal.Settings.Model.dll.
        winrt::Microsoft::Terminal::Settings::Model::AcpRuntimeState& _singleton()
        {
            static auto instance = winrt::make<AcpRuntimeState>();
            return instance;
        }

        std::wstring _agentKey(const winrt::hstring& agentId)
        {
            std::wstring key{ agentId };
            for (auto& ch : key)
            {
                ch = til::tolower_ascii(ch);
            }
            return key;
        }

        std::vector<Model::AcpModelInfo> _copyModels(
            const winrt::Windows::Foundation::Collections::IVectorView<Model::AcpModelInfo>& models)
        {
            std::vector<Model::AcpModelInfo> copy;
            if (models)
            {
                copy.reserve(models.Size());
                for (uint32_t i = 0; i < models.Size(); ++i)
                {
                    copy.push_back(models.GetAt(i));
                }
            }
            return copy;
        }
    }

    Microsoft::Terminal::Settings::Model::AcpRuntimeState AcpRuntimeState::Current()
    {
        return _singleton();
    }

    winrt::Windows::Foundation::Collections::IVectorView<Model::AcpModelInfo>
    AcpRuntimeState::AvailableModels(const winrt::hstring& agentId)
    {
        std::lock_guard lock{ _mutex };
        // Copy into a fresh vector view so callers don't observe later mutations
        // and we don't leak the internal storage.
        std::vector<Model::AcpModelInfo> snapshot;
        if (const auto it = _catalogs.find(_agentKey(agentId)); it != _catalogs.end())
        {
            snapshot = it->second.models;
        }
        return winrt::single_threaded_vector(std::move(snapshot)).GetView();
    }

    winrt::hstring AcpRuntimeState::CurrentModelId(const winrt::hstring& agentId)
    {
        std::lock_guard lock{ _mutex };
        if (const auto it = _catalogs.find(_agentKey(agentId)); it != _catalogs.end())
        {
            return it->second.currentId;
        }
        return {};
    }

    uint64_t AcpRuntimeState::Revision(const winrt::hstring& agentId)
    {
        std::lock_guard lock{ _mutex };
        if (const auto it = _catalogs.find(_agentKey(agentId)); it != _catalogs.end())
        {
            return it->second.revision;
        }
        return 0;
    }

    void AcpRuntimeState::SetAvailableModels(
        const winrt::hstring& agentId,
        const winrt::Windows::Foundation::Collections::IVectorView<Model::AcpModelInfo>& models,
        const winrt::hstring& currentId)
    {
        auto copy = _copyModels(models);
        {
            std::lock_guard lock{ _mutex };
            auto& catalog = _catalogs[_agentKey(agentId)];
            catalog.models = std::move(copy);
            catalog.currentId = currentId;
            ++catalog.revision;
        }
        // Fire outside the lock to avoid reentrant deadlocks if a handler
        // calls back into AvailableModels()/CurrentModelId().
        _changedEvent(*this, nullptr);
    }

    bool AcpRuntimeState::TrySetAvailableModels(
        const winrt::hstring& agentId,
        uint64_t expectedRevision,
        const winrt::Windows::Foundation::Collections::IVectorView<Model::AcpModelInfo>& models,
        const winrt::hstring& currentId)
    {
        auto copy = _copyModels(models);
        {
            std::lock_guard lock{ _mutex };
            auto key = _agentKey(agentId);
            auto it = _catalogs.find(key);
            if (it == _catalogs.end())
            {
                if (expectedRevision != 0)
                {
                    return false;
                }
                it = _catalogs.try_emplace(std::move(key)).first;
            }
            else if (it->second.revision != expectedRevision)
            {
                return false;
            }
            auto& catalog = it->second;
            catalog.models = std::move(copy);
            catalog.currentId = currentId;
            ++catalog.revision;
        }
        _changedEvent(*this, nullptr);
        return true;
    }

    winrt::event_token AcpRuntimeState::Changed(
        const winrt::Windows::Foundation::TypedEventHandler<
            Model::AcpRuntimeState,
            winrt::Windows::Foundation::IInspectable>& handler)
    {
        return _changedEvent.add(handler);
    }

    void AcpRuntimeState::Changed(const winrt::event_token& token) noexcept
    {
        _changedEvent.remove(token);
    }
}
