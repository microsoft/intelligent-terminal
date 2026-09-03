// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include <winrt/Windows.UI.Xaml.Documents.h>

#include "AIAgents.h"
#include "AIAgents.g.cpp"

using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Controls;
using namespace winrt::Windows::UI::Xaml::Documents;
using namespace winrt::Windows::UI::Xaml::Media;
using namespace winrt::Windows::UI::Xaml::Navigation;
using namespace winrt::Microsoft::Terminal::Settings::Model;

namespace winrt::Microsoft::Terminal::Settings::Editor::implementation
{
    AIAgents::AIAgents()
    {
        InitializeComponent();

        PageSubtitlePrefix().Text(RS_(L"AIAgents_PageSubtitlePrefix"));
        PageSubtitlePrivacyLink().Text(RS_(L"AIAgents_PageSubtitlePrivacyLink"));

        // Auto-error-detection caption + inline "supported shells" hyperlink.
        AutoErrorDetectionCaptionPrefix().Text(RS_(L"AIAgents_AutoErrorDetectionCaptionPrefix"));
        AutoErrorDetectionCaptionLink().Text(RS_(L"AIAgents_AutoErrorDetectionCaptionLink"));

        const auto agentHeader = RS_(L"AIAgents_AcpAgent/Header");
        AcpAgentHeaderText().Text(agentHeader);

        // Split the description on "ACP" (locked token) so it can be rendered as an inline Hyperlink.
        {
            const auto descStr = RS_(L"AIAgents_AcpAgent/HelpText");
            const std::wstring_view desc{ descStr };
            constexpr std::wstring_view token{ L"ACP" };
            const auto pos = desc.find(token);
            if (pos != std::wstring_view::npos)
            {
                AcpAgentDescriptionBefore().Text(winrt::hstring{ desc.substr(0, pos) });
                AcpAgentDescriptionAcpToken().Text(winrt::hstring{ token });
                AcpAgentDescriptionAfter().Text(winrt::hstring{ desc.substr(pos + token.size()) });
            }
            else
            {
                // Fallback (shouldn't happen — ACP is locked): degrade to plain text.
                AcpAgentDescriptionBefore().Text(winrt::hstring{ desc });
            }
        }

        Automation::AutomationProperties::SetName(AcpAgent(), agentHeader);
    }

    void AIAgents::CustomAgentRemove_Loaded(
        const IInspectable& sender,
        const RoutedEventArgs&)
    {
        const auto button = sender.as<Button>();
        for (auto parent = VisualTreeHelper::GetParent(button);
             parent;
             parent = VisualTreeHelper::GetParent(parent))
        {
            if (parent.try_as<ComboBoxItem>())
            {
                return;
            }
        }

        // ComboBox uses the same ItemTemplate for the collapsed selection.
        // Keep removal available only on entries inside the expanded list.
        button.Visibility(Visibility::Collapsed);
    }

    void AIAgents::OnNavigatedTo(const NavigationEventArgs& e)
    {
        const auto args = e.Parameter().as<Editor::NavigateToPageArgs>();
        _ViewModel = args.ViewModel().as<Editor::AIAgentsViewModel>();
        BringIntoViewWhenLoaded(args.ElementToFocus());
    }
}
