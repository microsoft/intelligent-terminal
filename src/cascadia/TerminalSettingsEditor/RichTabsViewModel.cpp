// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "RichTabsViewModel.h"
#include "RichTabFieldDescriptor.g.cpp"
#include "RichTabProviderDescriptor.g.cpp"
#include "RichTabFieldViewModel.g.cpp"
#include "RichTabConsentRequest.g.cpp"
#include "RichTabProviderViewModel.g.cpp"
#include "RichTabsViewModel.g.cpp"
#include "Utils.h"

#include <unordered_set>

namespace winrt::Microsoft::Terminal::Settings::Editor::implementation
{
    namespace
    {
        Model::RichTabProviderPreference _FindPreference(
            const Model::GlobalAppSettings& settings,
            const std::wstring_view id)
        {
            for (const auto& preference : settings.RichTabProviders())
            {
                if (preference.Id() == id)
                {
                    return preference;
                }
            }
            return nullptr;
        }

        Windows::Foundation::Collections::IVector<Model::RichTabProviderPreference> _MaterializeProviderPreferences(
            const Model::GlobalAppSettings& settings)
        {
            auto writable = single_threaded_vector<Model::RichTabProviderPreference>();
            for (const auto& preference : settings.RichTabProviders())
            {
                Model::RichTabProviderPreference copy{ preference.Id() };
                copy.Enabled(preference.Enabled());
                if (const auto fields = preference.Fields())
                {
                    auto copiedFields = single_threaded_vector<winrt::hstring>();
                    for (const auto& field : fields)
                    {
                        copiedFields.Append(field);
                    }
                    copy.Fields(copiedFields);
                }
                writable.Append(std::move(copy));
            }
            settings.RichTabProviders(writable);
            return writable;
        }

        void _MergeProviderOrder(
            const Model::GlobalAppSettings& settings,
            const std::vector<winrt::hstring>& providerOrder)
        {
            const auto existing = _MaterializeProviderPreferences(settings);
            std::unordered_set<std::wstring> knownIds;
            std::unordered_map<std::wstring, Model::RichTabProviderPreference> knownPreferences;
            for (const auto& id : providerOrder)
            {
                knownIds.emplace(id);
            }
            for (const auto& preference : existing)
            {
                const std::wstring id{ preference.Id() };
                if (knownIds.contains(id) && !knownPreferences.contains(id))
                {
                    knownPreferences.emplace(id, preference);
                }
            }

            std::vector<Model::RichTabProviderPreference> orderedKnown;
            orderedKnown.reserve(providerOrder.size());
            for (const auto& id : providerOrder)
            {
                const auto found = knownPreferences.find(std::wstring{ id });
                orderedKnown.emplace_back(
                    found == knownPreferences.end() ?
                        Model::RichTabProviderPreference{ id } :
                        found->second);
            }

            std::vector<Model::RichTabProviderPreference> merged;
            merged.reserve(existing.Size() + orderedKnown.size());
            size_t knownIndex = 0;
            for (const auto& preference : existing)
            {
                if (knownIds.contains(std::wstring{ preference.Id() }))
                {
                    if (knownIndex < orderedKnown.size())
                    {
                        merged.emplace_back(orderedKnown[knownIndex++]);
                    }
                }
                else
                {
                    merged.emplace_back(preference);
                }
            }
            merged.insert(merged.end(), orderedKnown.begin() + knownIndex, orderedKnown.end());
            existing.ReplaceAll(merged);
        }

        void _EnsureProviderPreferences(
            const Model::GlobalAppSettings& settings,
            const std::vector<winrt::hstring>& providerOrder)
        {
            const auto existing = _MaterializeProviderPreferences(settings);
            std::unordered_set<std::wstring> existingIds;
            for (const auto& preference : existing)
            {
                existingIds.emplace(preference.Id());
            }
            for (const auto& id : providerOrder)
            {
                if (existingIds.emplace(id).second)
                {
                    existing.Append(Model::RichTabProviderPreference{ id });
                }
            }
        }
    }

    RichTabProviderDescriptor::RichTabProviderDescriptor(
        winrt::hstring id,
        winrt::hstring displayName,
        const Editor::RichTabProviderSourceKind source,
        const bool consentEnabled,
        const bool integrityValid,
        const bool eligible,
        const bool effectiveEnabled,
        const bool shadowed,
        winrt::hstring consentKey,
        Windows::Foundation::Collections::IVectorView<Editor::RichTabFieldDescriptor> fields) :
        _id{ std::move(id) },
        _displayName{ std::move(displayName) },
        _source{ source },
        _consentEnabled{ consentEnabled },
        _integrityValid{ integrityValid },
        _eligible{ eligible },
        _effectiveEnabled{ effectiveEnabled },
        _shadowed{ shadowed },
        _consentKey{ std::move(consentKey) },
        _fields{ std::move(fields) }
    {
    }

    RichTabFieldViewModel::RichTabFieldViewModel(
        Editor::RichTabFieldDescriptor descriptor,
        const bool visible,
        const bool canEdit,
        std::function<void()> changed,
        std::function<void()> moveUp,
        std::function<void()> moveDown) :
        _descriptor{ std::move(descriptor) },
        _isVisible{ visible },
        _canEdit{ canEdit },
        _changed{ std::move(changed) },
        _moveUp{ std::move(moveUp) },
        _moveDown{ std::move(moveDown) }
    {
    }

    void RichTabFieldViewModel::IsVisible(const bool value)
    {
        if (_canEdit && _isVisible != value)
        {
            _isVisible = value;
            _NotifyChanges(L"IsVisible");
            _changed();
        }
    }

    void RichTabFieldViewModel::MoveUp()
    {
        if (_canMoveUp)
        {
            _moveUp();
        }
    }

    void RichTabFieldViewModel::MoveDown()
    {
        if (_canMoveDown)
        {
            _moveDown();
        }
    }

    void RichTabFieldViewModel::SetCanEdit(const bool canEdit)
    {
        if (_canEdit != canEdit)
        {
            _canEdit = canEdit;
            _NotifyChanges(L"CanEdit");
        }
    }

    void RichTabFieldViewModel::SetMoveState(const bool canMoveUp, const bool canMoveDown)
    {
        if (_canMoveUp != canMoveUp || _canMoveDown != canMoveDown)
        {
            _canMoveUp = canMoveUp;
            _canMoveDown = canMoveDown;
            _NotifyChanges(L"CanMoveUp", L"CanMoveDown");
        }
    }

    RichTabProviderViewModel::RichTabProviderViewModel(
        Model::GlobalAppSettings globalSettings,
        Editor::RichTabProviderDescriptor descriptor,
        std::function<void()> ensurePreference,
        std::function<void()> preferencesChanged,
        std::function<bool(const winrt::hstring&, bool, winrt::hstring&)> requestConsent) :
        _globalSettings{ std::move(globalSettings) },
        _descriptor{ std::move(descriptor) },
        _fields{ single_threaded_observable_vector<Editor::RichTabFieldViewModel>() },
        _ensurePreference{ std::move(ensurePreference) },
        _preferencesChanged{ std::move(preferencesChanged) },
        _requestConsent{ std::move(requestConsent) },
        _consentEnabled{ _descriptor.ConsentEnabled() }
    {
        const auto preference = _FindPreference(_globalSettings, _descriptor.Id());
        _isEnabled = _descriptor.EffectiveEnabled();
        const auto descriptorFields = _descriptor.Fields();
        std::unordered_set<std::wstring> added;
        if (preference && preference.Fields())
        {
            for (const auto& fieldId : preference.Fields())
            {
                _serializedFieldOrder.emplace_back(fieldId);
                const auto found = std::find_if(descriptorFields.begin(), descriptorFields.end(), [&](const auto& field) {
                    return field.Id() == fieldId;
                });
                if (found != descriptorFields.end())
                {
                    if (added.emplace(std::wstring{ fieldId }).second)
                    {
                        _fields.Append(winrt::make<RichTabFieldViewModel>(*found, true, _CanEditFields(), [] {}, [] {}, [] {}));
                    }
                }
            }
        }
        for (const auto& field : descriptorFields)
        {
            if (added.emplace(std::wstring{ field.Id() }).second)
            {
                const auto visible = !preference || !preference.Fields() ? field.DefaultVisible() : false;
                _fields.Append(winrt::make<RichTabFieldViewModel>(field, visible, _CanEditFields(), [] {}, [] {}, [] {}));
            }
        }
    }

    void RichTabProviderViewModel::InitializeFieldCallbacks()
    {
        const auto descriptors = _descriptor.Fields();
        std::vector<std::pair<Editor::RichTabFieldDescriptor, bool>> fields;
        fields.reserve(_fields.Size());
        for (const auto& field : _fields)
        {
            const auto descriptor = std::find_if(descriptors.begin(), descriptors.end(), [&](const auto& current) {
                return current.Id() == field.Id();
            });
            fields.emplace_back(*descriptor, field.IsVisible());
        }
        _fields.Clear();
        const auto weak = get_weak();
        for (const auto& [descriptor, visible] : fields)
        {
            Editor::RichTabFieldViewModel projected{ nullptr };
            auto implementation = winrt::make_self<RichTabFieldViewModel>(
                descriptor,
                visible,
                _CanEditFields(),
                [weak]() {
                    if (const auto self = weak.get())
                    {
                        self->_WriteFields();
                    }
                },
                [weak, id = descriptor.Id()]() {
                    if (const auto self = weak.get())
                    {
                        for (const auto& field : self->_fields)
                        {
                            if (field.Id() == id)
                            {
                                self->MoveFieldUp(field);
                                break;
                            }
                        }
                    }
                },
                [weak, id = descriptor.Id()]() {
                    if (const auto self = weak.get())
                    {
                        for (const auto& field : self->_fields)
                        {
                            if (field.Id() == id)
                            {
                                self->MoveFieldDown(field);
                                break;
                            }
                        }
                    }
                });
            projected = *implementation;
            _fields.Append(projected);
        }
        _UpdateFieldMoveState();
    }

    winrt::hstring RichTabProviderViewModel::SourceLabel() const
    {
        switch (_descriptor.Source())
        {
        case Editor::RichTabProviderSourceKind::BuiltIn:
            return RS_(L"RichTabs_SourceBuiltIn");
        case Editor::RichTabProviderSourceKind::AppExtension:
            return RS_(L"RichTabs_SourceAppExtension");
        case Editor::RichTabProviderSourceKind::Development:
            return RS_(L"RichTabs_SourceDevelopment");
        default:
            return RS_(L"RichTabs_SourceLegacyManaged");
        }
    }

    winrt::hstring RichTabProviderViewModel::StatusLabel() const
    {
        if (_descriptor.Shadowed())
        {
            return RS_(L"RichTabs_StatusShadowed");
        }
        if (!_descriptor.IntegrityValid())
        {
            return RS_(L"RichTabs_StatusIntegrityInvalid");
        }
        if (!_consentEnabled)
        {
            return RS_(L"RichTabs_StatusConsentRequired");
        }
        return _isEnabled ? RS_(L"RichTabs_StatusEnabled") : RS_(L"RichTabs_StatusDisabled");
    }

    Model::RichTabProviderPreference RichTabProviderViewModel::_Preference()
    {
        _ensurePreference();
        if (const auto existing = _FindPreference(_globalSettings, Id()))
        {
            return existing;
        }
        Model::RichTabProviderPreference preference{ Id() };
        _globalSettings.RichTabProviders().Append(preference);
        return preference;
    }

    void RichTabProviderViewModel::IsEnabled(const bool value)
    {
        if (CanToggle() && _isEnabled != value)
        {
            _isEnabled = value;
            _Preference().Enabled(winrt::box_value(value).as<Windows::Foundation::IReference<bool>>());
            _NotifyChanges(L"IsEnabled", L"StatusLabel");
            _preferencesChanged();
        }
    }

    bool RichTabProviderViewModel::CanToggle() const noexcept
    {
        if (_descriptor.Shadowed() || !_descriptor.IntegrityValid())
        {
            return false;
        }
        if (_descriptor.Eligible() ||
            (_consentEnabled &&
             _descriptor.Source() == Editor::RichTabProviderSourceKind::AppExtension &&
             !_descriptor.ConsentKey().empty()))
        {
            return true;
        }
        return !_consentEnabled &&
               _descriptor.Source() == Editor::RichTabProviderSourceKind::AppExtension &&
               !_descriptor.ConsentKey().empty();
    }

    winrt::hstring RichTabProviderViewModel::RequestConsent(const bool enabled)
    {
        if (!_requestConsent || !CanToggle() || !NeedsConsent())
        {
            return L"Provider consent cannot be changed";
        }

        winrt::hstring error;
        if (!_requestConsent(Id(), enabled, error))
        {
            return error.empty() ? winrt::hstring{ L"Provider consent could not be saved" } : error;
        }

        _consentEnabled = enabled;
        _isEnabled = enabled;
        _Preference().Enabled(winrt::box_value(enabled).as<Windows::Foundation::IReference<bool>>());
        for (const auto& field : _fields)
        {
            get_self<RichTabFieldViewModel>(field)->SetCanEdit(_CanEditFields());
        }
        _UpdateFieldMoveState();
        _NotifyChanges(L"NeedsConsent", L"CanToggle", L"IsEnabled", L"StatusLabel");
        _preferencesChanged();
        return {};
    }

    bool RichTabProviderViewModel::_CanEditFields() const noexcept
    {
        return _descriptor.Eligible() ||
               (_consentEnabled &&
                _descriptor.Source() == Editor::RichTabProviderSourceKind::AppExtension &&
                !_descriptor.Shadowed() &&
                _descriptor.IntegrityValid() &&
                !_descriptor.ConsentKey().empty());
    }

    void RichTabProviderViewModel::_WriteFields()
    {
        std::vector<winrt::hstring> visibleFields;
        for (const auto& field : _fields)
        {
            if (field.IsVisible())
            {
                visibleFields.emplace_back(field.Id());
            }
        }

        std::unordered_set<std::wstring> knownIds;
        for (const auto& field : _descriptor.Fields())
        {
            knownIds.emplace(field.Id());
        }
        std::vector<winrt::hstring> serialized;
        serialized.reserve(_serializedFieldOrder.size() + visibleFields.size());
        size_t visibleIndex = 0;
        for (const auto& field : _serializedFieldOrder)
        {
            if (knownIds.contains(std::wstring{ field }))
            {
                if (visibleIndex < visibleFields.size())
                {
                    serialized.emplace_back(visibleFields[visibleIndex++]);
                }
            }
            else
            {
                serialized.emplace_back(field);
            }
        }
        serialized.insert(serialized.end(), visibleFields.begin() + visibleIndex, visibleFields.end());
        _serializedFieldOrder = serialized;

        auto fields = single_threaded_vector<winrt::hstring>();
        fields.ReplaceAll(serialized);
        _Preference().Fields(fields);
        _preferencesChanged();
    }

    void RichTabProviderViewModel::MoveFieldUp(const Editor::RichTabFieldViewModel& field)
    {
        uint32_t index = 0;
        if (_fields.IndexOf(field, index) && index > 0)
        {
            _fields.RemoveAt(index);
            _fields.InsertAt(index - 1, field);
            _WriteFields();
            _UpdateFieldMoveState();
        }
    }

    void RichTabProviderViewModel::MoveFieldDown(const Editor::RichTabFieldViewModel& field)
    {
        uint32_t index = 0;
        if (_fields.IndexOf(field, index) && index + 1 < _fields.Size())
        {
            _fields.RemoveAt(index);
            _fields.InsertAt(index + 1, field);
            _WriteFields();
            _UpdateFieldMoveState();
        }
    }

    void RichTabProviderViewModel::_UpdateFieldMoveState()
    {
        const auto size = _fields.Size();
        for (uint32_t index = 0; index < size; ++index)
        {
            get_self<RichTabFieldViewModel>(_fields.GetAt(index))->SetMoveState(_CanEditFields() && index > 0, _CanEditFields() && index + 1 < size);
        }
    }

    void RichTabProviderViewModel::SetMoveState(const bool canMoveUp, const bool canMoveDown)
    {
        if (_canMoveUp != canMoveUp || _canMoveDown != canMoveDown)
        {
            _canMoveUp = canMoveUp;
            _canMoveDown = canMoveDown;
            _NotifyChanges(L"CanMoveUp", L"CanMoveDown");
        }
    }

    RichTabsViewModel::RichTabsViewModel(
        Model::GlobalAppSettings globalSettings,
        const Windows::Foundation::Collections::IVectorView<Editor::RichTabProviderDescriptor> descriptors) :
        _globalSettings{ std::move(globalSettings) },
        _providers{ single_threaded_observable_vector<Editor::RichTabProviderViewModel>() }
    {
        std::vector<Editor::RichTabProviderDescriptor> orderedDescriptors;
        std::unordered_set<std::wstring> added;
        orderedDescriptors.reserve(descriptors.Size());
        for (const auto& preference : _globalSettings.RichTabProviders())
        {
            for (const auto& descriptor : descriptors)
            {
                if (descriptor.Id() == preference.Id() && added.emplace(std::wstring{ descriptor.Id() }).second)
                {
                    orderedDescriptors.emplace_back(descriptor);
                }
            }
        }
        for (const auto& descriptor : descriptors)
        {
            if (added.emplace(std::wstring{ descriptor.Id() }).second)
            {
                orderedDescriptors.emplace_back(descriptor);
            }
        }

        std::vector<winrt::hstring> providerOrder;
        providerOrder.reserve(orderedDescriptors.size());
        for (const auto& descriptor : orderedDescriptors)
        {
            providerOrder.emplace_back(descriptor.Id());
        }
        for (const auto& descriptor : orderedDescriptors)
        {
            auto provider = winrt::make_self<RichTabProviderViewModel>(
                _globalSettings,
                descriptor,
                [settings = _globalSettings, providerOrder]() {
                    _EnsureProviderPreferences(settings, providerOrder);
                },
                [weak = get_weak(), settings = _globalSettings]() {
                    if (const auto self = weak.get())
                    {
                        self->PreferencesChanged.raise(*self, settings);
                    }
                },
                [weak = get_weak()](const winrt::hstring& id, const bool enabled, winrt::hstring& error) {
                    if (const auto self = weak.get())
                    {
                        const auto provider = std::find_if(
                            self->_providers.begin(),
                            self->_providers.end(),
                            [&](const auto& current) {
                                return current.Id() == id;
                            });
                        if (provider == self->_providers.end())
                        {
                            error = L"Provider is no longer in the Rich Tab catalog";
                            return false;
                        }
                        const auto request = winrt::make<RichTabConsentRequest>(
                            id,
                            get_self<RichTabProviderViewModel>(*provider)->ConsentKey(),
                            enabled);
                        self->ConsentRequested.raise(*self, request);
                        error = request.Error();
                        return request.Approved();
                    }
                    error = L"Rich Tab settings are no longer available";
                    return false;
                });
            provider->InitializeFieldCallbacks();
            _providers.Append(*provider);
        }
        _UpdateProviderMoveState();
    }

    bool RichTabsViewModel::PrioritizeRecentlyUpdatedFields() const noexcept
    {
        return _globalSettings.RichTabPrioritizeRecentlyUpdatedFields();
    }

    void RichTabsViewModel::PrioritizeRecentlyUpdatedFields(const bool value)
    {
        if (_globalSettings.RichTabPrioritizeRecentlyUpdatedFields() != value)
        {
            _globalSettings.RichTabPrioritizeRecentlyUpdatedFields(value);
            _NotifyChanges(L"PrioritizeRecentlyUpdatedFields");
            PreferencesChanged.raise(*this, _globalSettings);
        }
    }

    void RichTabsViewModel::MoveProviderUp(const Editor::RichTabProviderViewModel& provider)
    {
        uint32_t index = 0;
        if (_providers.IndexOf(provider, index) && index > 0)
        {
            _providers.RemoveAt(index);
            _providers.InsertAt(index - 1, provider);
            _WriteProviderOrder();
            _UpdateProviderMoveState();
            PreferencesChanged.raise(*this, _globalSettings);
        }
    }

    void RichTabsViewModel::MoveProviderDown(const Editor::RichTabProviderViewModel& provider)
    {
        uint32_t index = 0;
        if (_providers.IndexOf(provider, index) && index + 1 < _providers.Size())
        {
            _providers.RemoveAt(index);
            _providers.InsertAt(index + 1, provider);
            _WriteProviderOrder();
            _UpdateProviderMoveState();
            PreferencesChanged.raise(*this, _globalSettings);
        }
    }

    void RichTabsViewModel::_WriteProviderOrder()
    {
        std::vector<winrt::hstring> providerOrder;
        providerOrder.reserve(_providers.Size());
        std::unordered_set<std::wstring> added;
        for (const auto& provider : _providers)
        {
            if (added.emplace(std::wstring{ provider.Id() }).second)
            {
                providerOrder.emplace_back(provider.Id());
            }
        }
        _MergeProviderOrder(_globalSettings, providerOrder);
    }

    void RichTabsViewModel::_UpdateProviderMoveState()
    {
        const auto size = _providers.Size();
        for (uint32_t index = 0; index < size; ++index)
        {
            const auto provider = _providers.GetAt(index);
            get_self<RichTabProviderViewModel>(provider)->SetMoveState(
                index > 0,
                index + 1 < size);
        }
    }
}
