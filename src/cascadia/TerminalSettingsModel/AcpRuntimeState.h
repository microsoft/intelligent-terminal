// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "AcpRuntimeState.g.h"
#include "AcpModelInfo.g.h"

namespace winrt::Microsoft::Terminal::Settings::Model::implementation
{
    struct AcpModelInfo : AcpModelInfoT<AcpModelInfo>
    {
        AcpModelInfo(winrt::hstring id, winrt::hstring displayName, winrt::hstring description) :
            _id{ std::move(id) },
            _displayName{ std::move(displayName) },
            _description{ std::move(description) }
        {
        }

        winrt::hstring Id() const { return _id; }
        winrt::hstring DisplayName() const { return _displayName; }
        winrt::hstring Description() const { return _description; }

    private:
        winrt::hstring _id;
        winrt::hstring _displayName;
        winrt::hstring _description;
    };

    struct AcpRuntimeState : AcpRuntimeStateT<AcpRuntimeState>
    {
        AcpRuntimeState() = default;

        static Microsoft::Terminal::Settings::Model::AcpRuntimeState Current();

        winrt::Windows::Foundation::Collections::IVectorView<Model::AcpModelInfo> AvailableModels();
        winrt::hstring CurrentModelId();
        void SetAvailableModels(const winrt::Windows::Foundation::Collections::IVectorView<Model::AcpModelInfo>& models,
                                const winrt::hstring& currentId);

        winrt::event_token Changed(const winrt::Windows::Foundation::TypedEventHandler<
            Model::AcpRuntimeState,
            winrt::Windows::Foundation::IInspectable>& handler);
        void Changed(const winrt::event_token& token) noexcept;

    private:
        std::mutex _mutex;
        std::vector<Model::AcpModelInfo> _models;
        winrt::hstring _currentId;
        winrt::event<winrt::Windows::Foundation::TypedEventHandler<
            Model::AcpRuntimeState,
            winrt::Windows::Foundation::IInspectable>> _changedEvent;
    };
}

namespace winrt::Microsoft::Terminal::Settings::Model::factory_implementation
{
    BASIC_FACTORY(AcpModelInfo);
    BASIC_FACTORY(AcpRuntimeState);
}
