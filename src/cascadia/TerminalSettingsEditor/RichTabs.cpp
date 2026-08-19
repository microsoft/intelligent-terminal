// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "RichTabs.h"
#include "RichTabs.g.cpp"

using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Controls;
using namespace winrt::Windows::UI::Xaml::Navigation;

namespace winrt::Microsoft::Terminal::Settings::Editor::implementation
{
    RichTabs::RichTabs()
    {
        InitializeComponent();
    }

    void RichTabs::OnNavigatedTo(const NavigationEventArgs& e)
    {
        const auto args = e.Parameter().as<Editor::NavigateToPageArgs>();
        ViewModel(args.ViewModel().as<Editor::RichTabsViewModel>());
        BringIntoViewWhenLoaded(args.ElementToFocus());
    }

    void RichTabs::MoveProviderUp_Click(const IInspectable& sender, const RoutedEventArgs&)
    {
        ViewModel().MoveProviderUp(sender.as<Button>().Tag().as<Editor::RichTabProviderViewModel>());
    }

    void RichTabs::MoveProviderDown_Click(const IInspectable& sender, const RoutedEventArgs&)
    {
        ViewModel().MoveProviderDown(sender.as<Button>().Tag().as<Editor::RichTabProviderViewModel>());
    }

    void RichTabs::MoveFieldUp_Click(const IInspectable& sender, const RoutedEventArgs&)
    {
        sender.as<Button>().Tag().as<Editor::RichTabFieldViewModel>().MoveUp();
    }

    void RichTabs::MoveFieldDown_Click(const IInspectable& sender, const RoutedEventArgs&)
    {
        sender.as<Button>().Tag().as<Editor::RichTabFieldViewModel>().MoveDown();
    }

    safe_void_coroutine RichTabs::ProviderEnabled_Toggled(const IInspectable& sender, const RoutedEventArgs&)
    {
        const auto lifetime = get_strong();
        const auto toggle = sender.as<ToggleSwitch>();
        if (const auto viewModel = toggle.Tag().try_as<Editor::RichTabProviderViewModel>())
        {
            if (!toggle.IsOn() || !viewModel.NeedsConsent())
            {
                viewModel.IsEnabled(toggle.IsOn());
                co_return;
            }

            toggle.IsOn(false);
            ContentDialog confirmation;
            confirmation.XamlRoot(XamlRoot());
            confirmation.Title(box_value(RS_(L"RichTabs_ConsentDialogTitle")));
            confirmation.Content(box_value(RS_(L"RichTabs_ConsentDialogMessage")));
            confirmation.PrimaryButtonText(RS_(L"RichTabs_ConsentDialogAccept"));
            confirmation.CloseButtonText(RS_(L"RichTabs_ConsentDialogCancel"));
            confirmation.DefaultButton(ContentDialogButton::Close);
            if (co_await confirmation.ShowAsync() != ContentDialogResult::Primary)
            {
                co_return;
            }

            const auto error = viewModel.RequestConsent(true);
            if (error.empty())
            {
                toggle.IsOn(true);
                co_return;
            }

            ContentDialog failure;
            failure.XamlRoot(XamlRoot());
            failure.Title(box_value(RS_(L"RichTabs_ConsentDialogFailureTitle")));
            failure.Content(box_value(error));
            failure.CloseButtonText(RS_(L"RichTabs_ConsentDialogCancel"));
            co_await failure.ShowAsync();
        }
    }

    void RichTabs::FieldVisibility_Click(const IInspectable& sender, const RoutedEventArgs&)
    {
        const auto checkBox = sender.as<CheckBox>();
        if (const auto viewModel = checkBox.Tag().try_as<Editor::RichTabFieldViewModel>())
        {
            viewModel.IsVisible(checkBox.IsChecked().GetBoolean());
        }
    }
}
