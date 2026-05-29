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
