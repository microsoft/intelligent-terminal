// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

// The agent page subtitle uses inline <Run> + <Hyperlink> elements; we
// populate their Text from code-behind because x:Uid on inline Run is not
// reliably honored by ResourceLoader in this UWP/WinUI 2 build.
#include <winrt/Windows.UI.Xaml.Documents.h>

#include "AIAgents.h"
#include "AIAgents.g.cpp"

using namespace winrt::Windows::UI::Xaml::Navigation;
using namespace winrt::Microsoft::Terminal::Settings::Model;

namespace winrt::Microsoft::Terminal::Settings::Editor::implementation
{
    AIAgents::AIAgents()
    {
        InitializeComponent();

        PageSubtitlePrefix().Text(RS_(L"AIAgents_PageSubtitlePrefix"));
        PageSubtitlePrivacyLink().Text(RS_(L"AIAgents_PageSubtitlePrivacyLink"));
    }

    void AIAgents::OnNavigatedTo(const NavigationEventArgs& e)
    {
        const auto args = e.Parameter().as<Editor::NavigateToPageArgs>();
        _ViewModel = args.ViewModel().as<Editor::AIAgentsViewModel>();
        BringIntoViewWhenLoaded(args.ElementToFocus());
    }
}
