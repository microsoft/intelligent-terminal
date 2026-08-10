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

    void RichTabs::ProviderEnabled_Toggled(const IInspectable& sender, const RoutedEventArgs&)
    {
        const auto toggle = sender.as<ToggleSwitch>();
        if (const auto viewModel = toggle.Tag().try_as<Editor::RichTabProviderViewModel>())
        {
            viewModel.IsEnabled(toggle.IsOn());
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
