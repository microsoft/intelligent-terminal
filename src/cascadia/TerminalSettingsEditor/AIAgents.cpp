// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include <winrt/Windows.UI.Xaml.Documents.h>

#include "AIAgents.h"
#include "AIAgents.g.cpp"
#include "../inc/ShellIntegrationDiagnostics.h"

using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Controls;
using namespace winrt::Windows::UI::Xaml::Documents;
using namespace winrt::Windows::UI::Xaml::Navigation;
using namespace winrt::Microsoft::Terminal::Settings::Model;

namespace winrt::Microsoft::Terminal::Settings::Editor::implementation
{
    namespace
    {
        winrt::hstring _FormatRepairStatus(const winrt::hstring& localizedTemplate,
                                           const ::Microsoft::Terminal::ShellIntegration::Diagnostics::ShellTarget target)
        {
            std::wstring localized{ localizedTemplate.c_str(), localizedTemplate.size() };
            constexpr std::wstring_view placeholder{ L"{}" };
            if (const auto pos = localized.find(placeholder); pos != std::wstring::npos)
            {
                localized.replace(pos, placeholder.size(), ::Microsoft::Terminal::ShellIntegration::Diagnostics::ShellLabel(target));
            }
            return winrt::hstring{ localized };
        }

        winrt::hstring _BuildRepairStatusMessage(const ::Microsoft::Terminal::ShellIntegration::Diagnostics::ShellTarget target,
                                                 const winrt::hstring& persistedReason)
        {
            const auto reason = ::Microsoft::Terminal::ShellIntegration::Diagnostics::ParsePersistedRepairReason(
                std::wstring_view{ persistedReason.c_str(), persistedReason.size() });
            if (!reason.has_value())
            {
                return {};
            }

            switch (*reason)
            {
            case ::Microsoft::Terminal::ShellIntegration::Diagnostics::RepairReason::PromptChanged:
                return _FormatRepairStatus(RS_(L"AIAgents_ShellIntegrationRepairPromptChanged"), target);
            case ::Microsoft::Terminal::ShellIntegration::Diagnostics::RepairReason::RestartRequired:
                return _FormatRepairStatus(RS_(L"AIAgents_ShellIntegrationRepairRestartRequired"), target);
            case ::Microsoft::Terminal::ShellIntegration::Diagnostics::RepairReason::BindFailed:
                return _FormatRepairStatus(RS_(L"AIAgents_ShellIntegrationRepairBindFailed"), target);
            }

            return {};
        }

        void _SetRepairStatusTextBlock(const TextBlock& textBlock, const winrt::hstring& message)
        {
            if (!textBlock)
            {
                return;
            }

            textBlock.Text(message);
            textBlock.Visibility(message.empty() ? Visibility::Collapsed : Visibility::Visible);
        }
    }

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

    void AIAgents::OnNavigatedTo(const NavigationEventArgs& e)
    {
        const auto args = e.Parameter().as<Editor::NavigateToPageArgs>();
        _ViewModel = args.ViewModel().as<Editor::AIAgentsViewModel>();
        const auto state = ApplicationState::SharedInstance();
        const auto dispatcher = Dispatcher();
        _shellIntegrationRepairStateChangedRevoker = state.ShellIntegrationRepairStateChanged(
            winrt::auto_revoke,
            [weakThis = get_weak(), dispatcher](auto&&, auto&&) {
                dispatcher.RunAsync(
                    winrt::Windows::UI::Core::CoreDispatcherPriority::Normal,
                    [weakThis]() {
                        if (const auto page = weakThis.get())
                        {
                            page->_RefreshShellIntegrationRepairStatus();
                        }
                    });
            });
        _RefreshShellIntegrationRepairStatus();
        BringIntoViewWhenLoaded(args.ElementToFocus());
    }

    void AIAgents::OnNavigatedFrom(const NavigationEventArgs& /*e*/)
    {
        _shellIntegrationRepairStateChangedRevoker.revoke();
    }

    void AIAgents::_RefreshShellIntegrationRepairStatus()
    {
        const auto state = ApplicationState::SharedInstance();
        const auto pwshMessage = _BuildRepairStatusMessage(
            ::Microsoft::Terminal::ShellIntegration::Diagnostics::ShellTarget::Pwsh,
            state.PwshShellIntegrationRepairReason());
        const auto windowsPowerShellMessage = _BuildRepairStatusMessage(
            ::Microsoft::Terminal::ShellIntegration::Diagnostics::ShellTarget::WindowsPowerShell,
            state.WindowsPowerShellShellIntegrationRepairReason());

        _SetRepairStatusTextBlock(PwshShellIntegrationRepairStatus(), pwshMessage);
        _SetRepairStatusTextBlock(WindowsPowerShellShellIntegrationRepairStatus(), windowsPowerShellMessage);

        const auto anyVisible = !pwshMessage.empty() || !windowsPowerShellMessage.empty();
        AutoErrorDetectionRepairStatusPanel().Visibility(anyVisible ? Visibility::Visible : Visibility::Collapsed);
    }
}
