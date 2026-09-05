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
    static void _UpdateCustomAgentRemoveVisibility(const Button& button)
    {
        const auto entry = button.DataContext().try_as<Editor::AgentEntry>();
        bool isInExpandedList = false;
        for (auto parent = VisualTreeHelper::GetParent(button);
             parent;
             parent = VisualTreeHelper::GetParent(parent))
        {
            if (parent.try_as<ItemsPresenter>())
            {
                isInExpandedList = true;
                break;
            }
        }

        const auto visibility =
            isInExpandedList && entry ?
                entry.RemoveButtonVisibility() :
                Visibility::Collapsed;
        if (button.Visibility() != visibility)
        {
            button.Visibility(visibility);
        }
    }

    AIAgents::AIAgents()
    {
        InitializeComponent();

        PageSubtitlePrefix().Text(RS_(L"AIAgents_PageSubtitlePrefix"));
        PageSubtitlePrivacyLink().Text(RS_(L"AIAgents_PageSubtitlePrivacyLink"));

        const auto customModelsHeader = RS_(L"AIAgents_CustomModels/Header");
        const auto customModelsCaption = RS_(L"AIAgents_CustomModels/HelpText");
        const auto customModelsLearnMore = RS_(L"AIAgents_CustomModelsLearnMore");
        EmptyCustomModelsHeaderText().Text(customModelsHeader);
        EmptyCustomModelsCaptionPrefix().Text(customModelsCaption);
        EmptyCustomModelsCaptionLink().Text(customModelsLearnMore);
        CustomModelsHeaderText().Text(customModelsHeader);
        CustomModelsCaptionPrefix().Text(customModelsCaption);
        CustomModelsCaptionLink().Text(customModelsLearnMore);
        Automation::AutomationProperties::SetName(EmptyCustomModelProviders(), customModelsHeader);
        Automation::AutomationProperties::SetName(CustomModelProvidersExpander(), customModelsHeader);

        AutoErrorHandlingCaptionPrefix().Text(RS_(L"AIAgents_AutoErrorHandlingCaptionPrefix"));
        AutoErrorHandlingCaptionLink().Text(RS_(L"AIAgents_AutoErrorHandlingCaptionLink"));

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
        _UpdateCustomAgentRemoveVisibility(button);

        // ComboBox may reuse an expanded item's template for the collapsed
        // selection after Save. Register once per template instance and use
        // a weak reference so the handler does not extend its lifetime.
        if (!button.Tag())
        {
            button.Tag(box_value(true));
            const auto weakButton = make_weak(button);
            button.LayoutUpdated([weakButton](const auto&, const auto&) {
                if (const auto button = weakButton.get())
                {
                    _UpdateCustomAgentRemoveVisibility(button);
                }
            });
        }
    }

    void AIAgents::OnNavigatedTo(const NavigationEventArgs& e)
    {
        const auto args = e.Parameter().as<Editor::NavigateToPageArgs>();
        _ViewModel = args.ViewModel().as<Editor::AIAgentsViewModel>();
        BringIntoViewWhenLoaded(args.ElementToFocus());
    }
}
