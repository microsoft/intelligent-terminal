// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "AIAgents.g.h"
#include "Utils.h"

namespace winrt::Microsoft::Terminal::Settings::Editor::implementation
{
    struct AIAgents : public HasScrollViewer<AIAgents>, AIAgentsT<AIAgents>
    {
        AIAgents();

        void OnNavigatedTo(const winrt::Windows::UI::Xaml::Navigation::NavigationEventArgs& e);

        // Loaded handler for the AcpAgent / DelegateAgent SettingContainers.
        // Replaces the localized HelpText with the same text, but with the
        // literal substring "ACP" rewritten as a hyperlink to the ACP reference
        // page. "ACP" is {Locked} in every .resw file, so the split is locale-
        // independent and no per-locale resource changes are required.
        void _InjectAcpHelpTextLink(const winrt::Windows::Foundation::IInspectable& sender,
                                    const winrt::Windows::UI::Xaml::RoutedEventArgs& args);

        static double InstalledOpacity(bool isInstalled) { return isInstalled ? 1.0 : 0.4; }
        static bool NotBool(bool value) { return !value; }

        til::property_changed_event PropertyChanged;
        WINRT_OBSERVABLE_PROPERTY(Editor::AIAgentsViewModel, ViewModel, PropertyChanged.raise, nullptr);
    };
}

namespace winrt::Microsoft::Terminal::Settings::Editor::factory_implementation
{
    BASIC_FACTORY(AIAgents);
}
