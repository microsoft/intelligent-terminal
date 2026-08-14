// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "SettingsPaneContent.h"
#include "Utils.h"
#include "../RichTabProvider/ProviderBroker.h"

using namespace winrt::Windows::Foundation;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Microsoft::Terminal::Settings::Model;

#define ASSERT_UI_THREAD() assert(_sui.Dispatcher().HasThreadAccess())

namespace winrt::TerminalApp::implementation
{
    SettingsPaneContent::SettingsPaneContent(CascadiaSettings settings)
    {
        _sui = winrt::Microsoft::Terminal::Settings::Editor::MainPage{ settings };
        _UpdateRichTabProviderCatalog();
        _sui.RichTabPreferencesChanged([](auto&&, const auto& globalSettings) {
            _ApplyRichTabPreferences(globalSettings);
        });
        _sui.RichTabConsentRequested([weak = get_weak()](auto&&, const auto& request) {
            const auto self = weak.get();
            if (!self)
            {
                request.Error(L"Settings are no longer available");
                return;
            }
            namespace Provider = ::Microsoft::Terminal::RichTab::Provider;
            const auto result = Provider::ProviderBroker::Instance().SetProviderConsent(
                winrt::to_string(request.ProviderId()),
                winrt::to_string(request.ConsentKey()),
                request.Enabled());
            request.Approved(static_cast<bool>(result));
            if (!result)
            {
                std::string error;
                for (const auto& message : result.errors)
                {
                    if (!error.empty())
                    {
                        error.append("\n");
                    }
                    error.append(message);
                }
                request.Error(winrt::to_hstring(error));
                return;
            }
            self->_UpdateRichTabProviderCatalog();
        });

        // Stash away the current requested theme of the app. We'll need that in
        // _BackgroundBrush() to do a theme-aware resource lookup
        _requestedTheme = settings.GlobalSettings().CurrentTheme().RequestedTheme();
    }

    void SettingsPaneContent::_ApplyRichTabPreferences(const GlobalAppSettings& globalSettings)
    {
        namespace Provider = ::Microsoft::Terminal::RichTab::Provider;

        std::vector<Provider::ProviderPreference> preferences;
        const auto configured = globalSettings.RichTabProviders();
        preferences.reserve(configured.Size());
        for (const auto& provider : configured)
        {
            Provider::ProviderPreference preference;
            preference.id = winrt::to_string(provider.Id());
            if (const auto enabled = provider.Enabled())
            {
                preference.enabled = enabled.Value();
            }
            if (const auto fields = provider.Fields())
            {
                preference.fields.emplace();
                preference.fields->reserve(fields.Size());
                for (const auto& field : fields)
                {
                    preference.fields->emplace_back(winrt::to_string(field));
                }
            }
            preferences.emplace_back(std::move(preference));
        }
        Provider::ProviderBroker::Instance().ApplyPreferences(std::move(preferences));
    }

    void SettingsPaneContent::UpdateSettings(const CascadiaSettings& settings)
    {
        ASSERT_UI_THREAD();
        _UpdateRichTabProviderCatalog();
        _sui.UpdateSettings(settings);

        _requestedTheme = settings.GlobalSettings().CurrentTheme().RequestedTheme();
    }

    void SettingsPaneContent::_UpdateRichTabProviderCatalog()
    {
        namespace Provider = ::Microsoft::Terminal::RichTab::Provider;
        using EditorSource = winrt::Microsoft::Terminal::Settings::Editor::RichTabProviderSourceKind;

        auto descriptors = winrt::single_threaded_vector<winrt::Microsoft::Terminal::Settings::Editor::RichTabProviderDescriptor>();
        for (const auto& descriptor : Provider::ProviderBroker::Instance().Catalog())
        {
            auto fields = winrt::single_threaded_vector<winrt::Microsoft::Terminal::Settings::Editor::RichTabFieldDescriptor>();
            for (const auto& field : descriptor.fields)
            {
                fields.Append(winrt::Microsoft::Terminal::Settings::Editor::RichTabFieldDescriptor{
                    winrt::to_hstring(field.id),
                    winrt::to_hstring(field.displayName),
                    field.defaultVisible });
            }

            const auto source = descriptor.source == Provider::ProviderSourceKind::BuiltIn ?
                                    EditorSource::BuiltIn :
                                descriptor.source == Provider::ProviderSourceKind::AppExtension ?
                                    EditorSource::AppExtension :
                                descriptor.source == Provider::ProviderSourceKind::Development ?
                                    EditorSource::Development :
                                    EditorSource::LegacyManaged;
            descriptors.Append(winrt::Microsoft::Terminal::Settings::Editor::RichTabProviderDescriptor{
                winrt::to_hstring(descriptor.id),
                winrt::to_hstring(descriptor.displayName),
                source,
                descriptor.consentEnabled,
                descriptor.integrityValid,
                descriptor.eligible,
                descriptor.effectiveEnabled,
                descriptor.shadowed,
                winrt::to_hstring(descriptor.consentKey),
                fields.GetView() });
        }
        _sui.UpdateRichTabProviderCatalog(descriptors.GetView());
    }

    winrt::Windows::UI::Xaml::FrameworkElement SettingsPaneContent::GetRoot()
    {
        return _sui;
    }
    winrt::Windows::Foundation::Size SettingsPaneContent::MinimumSize()
    {
        return { 1, 1 };
    }
    void SettingsPaneContent::Focus(winrt::Windows::UI::Xaml::FocusState reason)
    {
        if (reason != FocusState::Unfocused)
        {
            _sui.as<Controls::Page>().Focus(reason);
        }
    }
    void SettingsPaneContent::Close()
    {
    }

    INewContentArgs SettingsPaneContent::GetNewTerminalArgs(const BuildStartupKind /*kind*/) const
    {
        return BaseContentArgs(L"settings");
    }

    winrt::hstring SettingsPaneContent::Icon() const
    {
        // This is the Setting icon (looks like a gear)
        static constexpr std::wstring_view glyph{ L"\xE713" };
        return winrt::hstring{ glyph };
    }

    Windows::Foundation::IReference<winrt::Windows::UI::Color> SettingsPaneContent::TabColor() const noexcept
    {
        return nullptr;
    }

    winrt::Windows::UI::Xaml::Media::Brush SettingsPaneContent::BackgroundBrush()
    {
        // Look up the color we should use for the settings tab item from our
        // resources. This should only be used for when "terminalBackground" is
        // requested.
        static const auto key = winrt::box_value(L"SettingsUiTabBrush");
        // You can't just do a Application::Current().Resources().TryLookup
        // lookup, cause the app theme never changes! Do the hacky version
        // instead.
        return ThemeLookup(Application::Current().Resources(), _requestedTheme, key).try_as<winrt::Windows::UI::Xaml::Media::Brush>();
    }
}
