// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

// The agent page subtitle uses inline <Run> + <Hyperlink> elements; we
// populate their Text from code-behind because x:Uid on inline Run is not
// reliably honored by ResourceLoader in this UWP/WinUI 2 build.
#include <winrt/Windows.UI.Xaml.Documents.h>
#include <winrt/Windows.UI.Xaml.Media.h>
#include <winrt/Windows.UI.Xaml.Controls.h>
#include <winrt/Windows.Foundation.h>

#include "AIAgents.h"
#include "AIAgents.g.cpp"

#include "../inc/AcpLinkInjector.h"

using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Controls;
using namespace winrt::Windows::UI::Xaml::Documents;
using namespace winrt::Windows::UI::Xaml::Media;
using namespace winrt::Windows::UI::Xaml::Navigation;
using namespace winrt::Microsoft::Terminal::Settings::Model;

namespace
{
    // Walk the visual tree below `root` looking for a FrameworkElement whose
    // Name() matches `name`. Returns nullptr if not found.
    FrameworkElement _FindDescendantByName(const DependencyObject& root, std::wstring_view name)
    {
        if (!root)
        {
            return nullptr;
        }
        const auto count = VisualTreeHelper::GetChildrenCount(root);
        for (int i = 0; i < count; ++i)
        {
            const auto child = VisualTreeHelper::GetChild(root, i);
            if (const auto fe = child.try_as<FrameworkElement>())
            {
                if (fe.Name() == name)
                {
                    return fe;
                }
            }
            if (auto found = _FindDescendantByName(child, name))
            {
                return found;
            }
        }
        return nullptr;
    }
}

namespace winrt::Microsoft::Terminal::Settings::Editor::implementation
{
    AIAgents::AIAgents()
    {
        InitializeComponent();

        PageSubtitlePrefix().Text(RS_(L"AIAgents_PageSubtitlePrefix"));
        PageSubtitlePrivacyLink().Text(RS_(L"AIAgents_PageSubtitlePrivacyLink"));

        // Hook Loaded on both SettingContainers so we can rewrite their
        // template's HelpTextBlock once the localized HelpText TemplateBinding
        // has populated it.
        AcpAgent().Loaded({ this, &AIAgents::_InjectAcpHelpTextLink });
        DelegateAgent().Loaded({ this, &AIAgents::_InjectAcpHelpTextLink });
    }

    void AIAgents::OnNavigatedTo(const NavigationEventArgs& e)
    {
        const auto args = e.Parameter().as<Editor::NavigateToPageArgs>();
        _ViewModel = args.ViewModel().as<Editor::AIAgentsViewModel>();
        BringIntoViewWhenLoaded(args.ElementToFocus());
    }

    void AIAgents::_InjectAcpHelpTextLink(const winrt::Windows::Foundation::IInspectable& sender,
                                          const winrt::Windows::UI::Xaml::RoutedEventArgs& /*args*/)
    {
        const auto container = sender.try_as<DependencyObject>();
        if (!container)
        {
            return;
        }
        // "HelpTextBlock" is the template-part name used by every SettingContainer
        // ControlTemplate variant in SettingContainerStyle.xaml.
        if (const auto helpTextBlock = _FindDescendantByName(container, L"HelpTextBlock").try_as<TextBlock>())
        {
            ::Microsoft::Terminal::AcpLink::InjectAcpLink(helpTextBlock);
        }
    }
}
