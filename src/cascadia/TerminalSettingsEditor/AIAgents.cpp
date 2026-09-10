// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include <limits>
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
    static void _FormatInlineShortcuts(const RichTextBlock& textBlock, std::wstring_view text, const DataTemplate& keyChordTemplate)
    {
        constexpr std::wstring_view promptShortcut{ L"Alt+Shift+/" };
        constexpr std::wstring_view directShortcut{ L"Alt+Shift+B" };
        textBlock.Blocks().Clear();
        while (!text.empty())
        {
            const auto newline = text.find(L'\n');
            auto line = text.substr(0, newline);
            Paragraph paragraph;
            const auto inlines = paragraph.Inlines();
            const auto appendText = [&](const std::wstring_view value) {
                if (!value.empty())
                {
                    Run run;
                    run.Text(winrt::hstring{ value });
                    inlines.Append(run);
                }
            };
            while (!line.empty())
            {
                const auto promptPos = line.find(promptShortcut);
                const auto directPos = line.find(directShortcut);
                const auto pos = std::min(promptPos, directPos);
                if (pos == std::wstring_view::npos)
                {
                    appendText(line);
                    break;
                }

                const auto shortcut = promptPos <= directPos ? promptShortcut : directShortcut;
                appendText(line.substr(0, pos));
                const auto badge = keyChordTemplate.LoadContent().as<Border>();
                const auto label = badge.Child().as<TextBlock>();
                label.Text(winrt::hstring{ shortcut });
                // Establish the font metrics before the rich-text layout consumes the keycap.
                badge.Measure({ std::numeric_limits<float>::infinity(), std::numeric_limits<float>::infinity() });
                const auto align = [weakBadge = make_weak(badge), weakParagraph = make_weak(paragraph)](const auto&, const auto&) {
                    const auto badge = weakBadge.get();
                    const auto paragraph = weakParagraph.get();
                    if (badge && paragraph)
                    {
                        const auto label = badge.Child().as<TextBlock>();
                        const auto padding = badge.Padding();
                        const auto stroke = badge.BorderThickness();
                        const auto textHeight = label.ActualHeight() > 0 ? label.ActualHeight() : label.DesiredSize().Height;
                        auto margin = badge.Margin();
                        const auto bottom = std::min(0.0, label.BaselineOffset() - textHeight - padding.Bottom - stroke.Bottom);
                        if (margin.Bottom != bottom)
                        {
                            margin.Bottom = bottom;
                            badge.Margin(margin);
                        }
                        // Reserve the entire keycap height when shortcut text wraps.
                        const auto height = textHeight + padding.Top + padding.Bottom + stroke.Top + stroke.Bottom;
                        if (paragraph.LineHeight() != height)
                        {
                            paragraph.LineHeight(height);
                        }
                    }
                };
                badge.Loaded(align);
                badge.SizeChanged(align);
                label.Loaded(align);
                label.SizeChanged(align);
                align(nullptr, nullptr);
                InlineUIContainer inlineBadge;
                inlineBadge.Child(badge);
                inlines.Append(inlineBadge);
                line.remove_prefix(pos + shortcut.size());
            }

            textBlock.Blocks().Append(paragraph);
            if (newline == std::wstring_view::npos)
            {
                break;
            }
            text.remove_prefix(newline + 1);
        }
    }

    static void _AlignInlineLinkText(const TextBlock& text)
    {
        for (auto parent = VisualTreeHelper::GetParent(text);
             parent;
             parent = VisualTreeHelper::GetParent(parent))
        {
            if (const auto link = parent.try_as<HyperlinkButton>())
            {
                // InlineUIContainer puts the child's bottom on the text baseline.
                // Let the measured font descent extend below it instead of using a pixel offset.
                const auto bottom = std::min(0.0, text.BaselineOffset() - text.ActualHeight());
                auto margin = link.Margin();
                if (margin.Bottom != bottom)
                {
                    margin.Bottom = bottom;
                    link.Margin(margin);
                }
                break;
            }
        }
    }

    static void _UpdateCustomAgentActionVisibility(const Button& button)
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
        CustomModelsHeaderText().Text(customModelsHeader);
        CustomModelsCaptionPrefix().Text(customModelsCaption);
        CustomModelsCaptionLink().Text(customModelsLearnMore);
        Automation::AutomationProperties::SetName(CustomModelProvidersExpander(), customModelsHeader);

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

    void AIAgents::InlineLinkText_Loaded(const IInspectable& sender, const RoutedEventArgs&)
    {
        _AlignInlineLinkText(sender.as<TextBlock>());
    }

    void AIAgents::InlineLinkText_SizeChanged(const IInspectable& sender, const SizeChangedEventArgs&)
    {
        _AlignInlineLinkText(sender.as<TextBlock>());
    }

    void AIAgents::CustomProviderForm_SizeChanged(const IInspectable&, const SizeChangedEventArgs& args)
    {
        const auto width = args.NewSize().Width;
        CustomProviderBaseUrlBox().MaxWidth(width);
        CustomProviderModelIdBox().MaxWidth(width);
        CustomProviderApiKeyBox().MaxWidth(width);
    }

    void AIAgents::DelegateAgentHelp_Loaded(const IInspectable& sender, const RoutedEventArgs&)
    {
        const auto text = sender.as<RichTextBlock>();
        const auto description = unbox_value<winrt::hstring>(text.Tag());
        const auto keyChordTemplate = Resources().Lookup(box_value(L"KeyChordLabelTemplate")).as<DataTemplate>();
        _FormatInlineShortcuts(text, description, keyChordTemplate);
    }

    void AIAgents::CustomAgentEdit_Click(const IInspectable& sender, const RoutedEventArgs&)
    {
        const auto button = sender.as<Button>();
        const auto entry = button.DataContext().try_as<Editor::AgentEntry>();
        const auto viewModel = ViewModel();
        if (!entry || entry.RemoveButtonVisibility() != Visibility::Visible)
        {
            LOG_HR(E_INVALIDARG);
            return;
        }
        if (viewModel.IsCustomAgentPolicyLocked())
        {
            LOG_HR(E_ACCESSDENIED);
            return;
        }

        ComboBox owner{ nullptr };
        for (auto parent = VisualTreeHelper::GetParent(button);
             parent;
             parent = VisualTreeHelper::GetParent(parent))
        {
            if (const auto item = parent.try_as<ComboBoxItem>())
            {
                owner = ItemsControl::ItemsControlFromItemContainer(item).try_as<ComboBox>();
                break;
            }
        }
        if (!owner || (owner != AcpAgentComboBox() && owner != DelegateAgentComboBox()))
        {
            LOG_HR(E_UNEXPECTED);
            return;
        }

        owner.IsDropDownOpen(false);
        if (owner == AcpAgentComboBox())
        {
            viewModel.CurrentAcpAgent(entry);
            viewModel.EditCustomAcpAgent();
            CustomAcpCommandBox().Focus(FocusState::Programmatic);
        }
        else
        {
            viewModel.CurrentDelegateAgent(entry);
            viewModel.EditCustomDelegateAgent();
            CustomDelegateCommandBox().Focus(FocusState::Programmatic);
        }
    }

    void AIAgents::CustomAgentAction_Loaded(
        const IInspectable& sender,
        const RoutedEventArgs&)
    {
        const auto button = sender.as<Button>();
        _UpdateCustomAgentActionVisibility(button);

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
                    _UpdateCustomAgentActionVisibility(button);
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
