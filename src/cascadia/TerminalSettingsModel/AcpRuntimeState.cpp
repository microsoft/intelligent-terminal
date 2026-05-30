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
    }

    Microsoft::Terminal::Settings::Model::AcpRuntimeState AcpRuntimeState::Current()
    {
        return _singleton();
    }

    winrt::Windows::Foundation::Collections::IVectorView<Model::AcpModelInfo>
    AcpRuntimeState::AvailableModels()
    {
        std::lock_guard lock{ _mutex };
        // Copy into a fresh vector view so callers don't observe later mutations
        // and we don't leak the internal storage.
        std::vector<Model::AcpModelInfo> snapshot{ _models };
        return winrt::single_threaded_vector(std::move(snapshot)).GetView();
    }

    winrt::hstring AcpRuntimeState::CurrentModelId()
    {
        std::lock_guard lock{ _mutex };
        return _currentId;
    }

    void AcpRuntimeState::SetAvailableModels(
        const winrt::Windows::Foundation::Collections::IVectorView<Model::AcpModelInfo>& models,
        const winrt::hstring& currentId)
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
        {
            std::lock_guard lock{ _mutex };
            _models = std::move(copy);
            _currentId = currentId;
        }
        // Fire outside the lock to avoid reentrant deadlocks if a handler
        // calls back into AvailableModels()/CurrentModelId().
        _changedEvent(*this, nullptr);
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
