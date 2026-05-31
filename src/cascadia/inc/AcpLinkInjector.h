// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// AcpLinkInjector.h
//
// Shared helper that rewrites a localized TextBlock's text so the literal
// substring "ACP" becomes a hyperlink to the Intelligent Terminal ACP
// reference page (https://aka.ms/intelligent-terminal-acpref).
//
// Used in two places:
//   - The AI agents settings page (TerminalSettingsEditor::AIAgents) —
//     applied to each SettingContainer's template "HelpTextBlock" part.
//   - The first-run experience agent picker
//     (TerminalApp::FreOverlay) — applied directly to a named TextBlock.
//
// Why this works for every locale with zero per-locale resource churn:
// "ACP" is marked `{Locked="ACP"}` in every locale's `.resw` comment for
// the relevant strings, so every translated sentence is guaranteed to
// contain the literal substring "ACP". Splitting the localized text on
// that token at runtime gives us a per-locale-correct prefix and suffix
// without adding any new resource keys.
//
// Header-only so neither consumer has to take a link-time dependency on
// a shared static library.

#pragma once

#include <string>
#include <string_view>

#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.UI.Xaml.Controls.h>
#include <winrt/Windows.UI.Xaml.Documents.h>

namespace Microsoft::Terminal::AcpLink
{
    // Replace `target`'s text with the same content, but with the literal
    // substring "ACP" turned into a hyperlink. Idempotent and defensive — a
    // no-op when:
    //   - `target` is null,
    //   - text is empty (e.g. HelpText not yet bound),
    //   - no "ACP" substring is found (locale anomaly — leave text alone),
    //   - a Hyperlink is already present (re-Loaded / re-Initialize firings).
    inline void InjectAcpLink(const winrt::Windows::UI::Xaml::Controls::TextBlock& target)
    {
        if (!target)
        {
            return;
        }

        // Idempotency: if any inline is already a Hyperlink, we've injected.
        const auto existingInlines = target.Inlines();
        for (const auto& inl : existingInlines)
        {
            if (inl.try_as<winrt::Windows::UI::Xaml::Documents::Hyperlink>())
            {
                return;
            }
        }

        const std::wstring fullText{ target.Text() };
        if (fullText.empty())
        {
            return;
        }
        constexpr std::wstring_view acpToken{ L"ACP" };
        const auto pos = fullText.find(acpToken);
        if (pos == std::wstring::npos)
        {
            return;
        }

        // Setting Text("") then mutating Inlines avoids the implicit single-Run
        // that Text() would otherwise re-create.
        target.Text(L"");
        auto inlines = target.Inlines();
        inlines.Clear();

        if (pos > 0)
        {
            winrt::Windows::UI::Xaml::Documents::Run prefix;
            prefix.Text(winrt::hstring{ fullText.substr(0, pos) });
            inlines.Append(prefix);
        }

        winrt::Windows::UI::Xaml::Documents::Hyperlink link;
        link.NavigateUri(winrt::Windows::Foundation::Uri{ L"https://aka.ms/intelligent-terminal-acpref" });
        winrt::Windows::UI::Xaml::Documents::Run linkRun;
        linkRun.Text(L"ACP");
        link.Inlines().Append(linkRun);
        inlines.Append(link);

        const auto suffixStart = pos + acpToken.size();
        if (suffixStart < fullText.size())
        {
            winrt::Windows::UI::Xaml::Documents::Run suffix;
            suffix.Text(winrt::hstring{ fullText.substr(suffixStart) });
            inlines.Append(suffix);
        }
    }
}
