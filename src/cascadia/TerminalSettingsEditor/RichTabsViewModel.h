// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "RichTabFieldDescriptor.g.h"
#include "RichTabProviderDescriptor.g.h"
#include "RichTabFieldViewModel.g.h"
#include "RichTabProviderViewModel.g.h"
#include "RichTabsViewModel.g.h"
#include "ViewModelHelpers.h"

namespace winrt::Microsoft::Terminal::Settings::Editor::implementation
{
    struct RichTabFieldDescriptor : RichTabFieldDescriptorT<RichTabFieldDescriptor>
    {
        RichTabFieldDescriptor(winrt::hstring id, winrt::hstring displayName, bool defaultVisible) :
            _id{ std::move(id) },
            _displayName{ std::move(displayName) },
            _defaultVisible{ defaultVisible }
        {
        }

        winrt::hstring Id() const noexcept { return _id; }
        winrt::hstring DisplayName() const noexcept { return _displayName; }
        bool DefaultVisible() const noexcept { return _defaultVisible; }

    private:
        winrt::hstring _id;
        winrt::hstring _displayName;
        bool _defaultVisible{ false };
    };

    struct RichTabProviderDescriptor : RichTabProviderDescriptorT<RichTabProviderDescriptor>
    {
        RichTabProviderDescriptor(
            winrt::hstring id,
            winrt::hstring displayName,
            Editor::RichTabProviderSourceKind source,
            bool consentEnabled,
            bool integrityValid,
            bool eligible,
            bool effectiveEnabled,
            bool shadowed,
            Windows::Foundation::Collections::IVectorView<Editor::RichTabFieldDescriptor> fields);

        winrt::hstring Id() const noexcept { return _id; }
        winrt::hstring DisplayName() const noexcept { return _displayName; }
        Editor::RichTabProviderSourceKind Source() const noexcept { return _source; }
        bool ConsentEnabled() const noexcept { return _consentEnabled; }
        bool IntegrityValid() const noexcept { return _integrityValid; }
        bool Eligible() const noexcept { return _eligible; }
        bool EffectiveEnabled() const noexcept { return _effectiveEnabled; }
        bool Shadowed() const noexcept { return _shadowed; }
        Windows::Foundation::Collections::IVectorView<Editor::RichTabFieldDescriptor> Fields() const noexcept { return _fields; }

    private:
        winrt::hstring _id;
        winrt::hstring _displayName;
        Editor::RichTabProviderSourceKind _source{ Editor::RichTabProviderSourceKind::Managed };
        bool _consentEnabled{ false };
        bool _integrityValid{ false };
        bool _eligible{ false };
        bool _effectiveEnabled{ false };
        bool _shadowed{ false };
        Windows::Foundation::Collections::IVectorView<Editor::RichTabFieldDescriptor> _fields{ nullptr };
    };

    struct RichTabFieldViewModel :
        RichTabFieldViewModelT<RichTabFieldViewModel>,
        ViewModelHelper<RichTabFieldViewModel>
    {
        RichTabFieldViewModel(
            Editor::RichTabFieldDescriptor descriptor,
            bool visible,
            bool canEdit,
            std::function<void()> changed,
            std::function<void()> moveUp,
            std::function<void()> moveDown);

        using ViewModelHelper<RichTabFieldViewModel>::PropertyChanged;

        winrt::hstring Id() const noexcept { return _descriptor.Id(); }
        winrt::hstring DisplayName() const noexcept { return _descriptor.DisplayName(); }
        bool IsVisible() const noexcept { return _isVisible; }
        void IsVisible(bool value);
        bool CanEdit() const noexcept { return _canEdit; }
        bool CanMoveUp() const noexcept { return _canMoveUp; }
        bool CanMoveDown() const noexcept { return _canMoveDown; }
        void MoveUp();
        void MoveDown();
        void SetMoveState(bool canMoveUp, bool canMoveDown);

    private:
        Editor::RichTabFieldDescriptor _descriptor;
        bool _isVisible{ false };
        bool _canEdit{ false };
        bool _canMoveUp{ false };
        bool _canMoveDown{ false };
        std::function<void()> _changed;
        std::function<void()> _moveUp;
        std::function<void()> _moveDown;
    };

    struct RichTabProviderViewModel :
        RichTabProviderViewModelT<RichTabProviderViewModel>,
        ViewModelHelper<RichTabProviderViewModel>
    {
        RichTabProviderViewModel(
            Model::GlobalAppSettings globalSettings,
            Editor::RichTabProviderDescriptor descriptor,
            std::function<void()> ensurePreference,
            std::function<void()> preferencesChanged);

        using ViewModelHelper<RichTabProviderViewModel>::PropertyChanged;

        void InitializeFieldCallbacks();
        winrt::hstring Id() const noexcept { return _descriptor.Id(); }
        winrt::hstring DisplayName() const noexcept { return _descriptor.DisplayName(); }
        winrt::hstring SourceLabel() const;
        winrt::hstring StatusLabel() const;
        bool IsEnabled() const noexcept { return _isEnabled; }
        void IsEnabled(bool value);
        bool CanToggle() const noexcept { return _descriptor.Eligible(); }
        bool CanMoveUp() const noexcept { return _canMoveUp; }
        bool CanMoveDown() const noexcept { return _canMoveDown; }
        Windows::Foundation::Collections::IObservableVector<Editor::RichTabFieldViewModel> Fields() const noexcept { return _fields; }
        void MoveFieldUp(const Editor::RichTabFieldViewModel& field);
        void MoveFieldDown(const Editor::RichTabFieldViewModel& field);
        void SetMoveState(bool canMoveUp, bool canMoveDown);

    private:
        Model::RichTabProviderPreference _Preference();
        void _WriteFields();
        void _UpdateFieldMoveState();

        Model::GlobalAppSettings _globalSettings;
        Editor::RichTabProviderDescriptor _descriptor;
        Windows::Foundation::Collections::IObservableVector<Editor::RichTabFieldViewModel> _fields;
        std::vector<winrt::hstring> _serializedFieldOrder;
        std::function<void()> _ensurePreference;
        std::function<void()> _preferencesChanged;
        bool _isEnabled{ false };
        bool _canMoveUp{ false };
        bool _canMoveDown{ false };
    };

    struct RichTabsViewModel :
        RichTabsViewModelT<RichTabsViewModel>,
        ViewModelHelper<RichTabsViewModel>
    {
        RichTabsViewModel(
            Model::GlobalAppSettings globalSettings,
            Windows::Foundation::Collections::IVectorView<Editor::RichTabProviderDescriptor> descriptors);

        using ViewModelHelper<RichTabsViewModel>::PropertyChanged;

        Windows::Foundation::Collections::IObservableVector<Editor::RichTabProviderViewModel> Providers() const noexcept { return _providers; }
        void MoveProviderUp(const Editor::RichTabProviderViewModel& provider);
        void MoveProviderDown(const Editor::RichTabProviderViewModel& provider);

        til::typed_event<Editor::RichTabsViewModel, Model::GlobalAppSettings> PreferencesChanged;

    private:
        void _WriteProviderOrder();
        void _UpdateProviderMoveState();

        Model::GlobalAppSettings _globalSettings;
        Windows::Foundation::Collections::IObservableVector<Editor::RichTabProviderViewModel> _providers;
    };
}

namespace winrt::Microsoft::Terminal::Settings::Editor::factory_implementation
{
    BASIC_FACTORY(RichTabFieldDescriptor);
    BASIC_FACTORY(RichTabProviderDescriptor);
    BASIC_FACTORY(RichTabsViewModel);
}
