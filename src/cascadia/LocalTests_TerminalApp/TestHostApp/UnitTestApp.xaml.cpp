//-----------------------------------------------------------
// Copyright (c) Microsoft Corporation. All Rights Reserved.
//-----------------------------------------------------------
#include "pch.h"

namespace TestHostApp
{
    static bool _IsAccessibilitySurface(Platform::String ^ arguments, const wchar_t* surface)
    {
        return arguments != nullptr && wcscmp(arguments->Data(), surface) == 0;
    }

    static void _PrepareAccessibilityWindow(Platform::String ^ title)
    {
        const auto view = Windows::UI::ViewManagement::ApplicationView::GetForCurrentView();
        view->Title = title;
        view->SetPreferredMinSize({ 800, 600 });
        Windows::UI::ViewManagement::ApplicationView::PreferredLaunchViewSize = { 1200, 900 };
        Windows::UI::ViewManagement::ApplicationView::PreferredLaunchWindowingMode =
            Windows::UI::ViewManagement::ApplicationViewWindowingMode::PreferredLaunchViewSize;
    }

    /// <summary>
    /// Initializes the singleton application object.  This is the first line of authored code
    /// executed, and as such is the logical equivalent of main() or WinMain().
    /// </summary>
    App::App()
    {
        InitializeComponent();
    }

    /// <summary>
    /// Invoked when the application is launched normally by the end user.    Other entry points
    /// will be used such as when the application is launched to open a specific file.
    /// </summary>
    /// <param name="e">Details about the launch request and process.</param>
    void App::OnLaunched(Windows::ApplicationModel::Activation::LaunchActivatedEventArgs ^ e)
    {
        if (_IsAccessibilitySurface(e->Arguments, L"--accessibility-page=fre"))
        {
            _PrepareAccessibilityWindow(L"Accessibility Test Host - FRE");
            const auto settings = Microsoft::Terminal::Settings::Model::CascadiaSettings::LoadDefaults();
            const auto overlay = ref new TerminalApp::FreOverlay();
            overlay->Initialize(settings);
            Windows::UI::Xaml::Window::Current->Content = overlay;
            Windows::UI::Xaml::Window::Current->Activate();
            return;
        }

        if (_IsAccessibilitySurface(e->Arguments, L"--accessibility-page=agents"))
        {
            _PrepareAccessibilityWindow(L"Accessibility Test Host - Agents settings");
            const auto settings = Microsoft::Terminal::Settings::Model::CascadiaSettings::LoadDefaults();
            const auto page = ref new Microsoft::Terminal::Settings::Editor::AIAgents(settings->GlobalSettings);
            Windows::UI::Xaml::Window::Current->Content = page;
            Windows::UI::Xaml::Window::Current->Activate();
            return;
        }

        Windows::UI::Xaml::Window::Current->Activate();
        Microsoft::VisualStudio::TestPlatform::TestExecutor::WinRTCore::UnitTestClient::Run(e->Arguments);
    }
}
