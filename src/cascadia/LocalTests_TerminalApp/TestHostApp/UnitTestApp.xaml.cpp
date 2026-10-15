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

    static void _ShowAccessibilityStartupError(Platform::Exception ^ error)
    {
        const auto errorText = ref new Windows::UI::Xaml::Controls::TextBlock();
        Windows::UI::Xaml::Automation::AutomationProperties::SetAutomationId(errorText, L"TestHostStartupError");
        errorText->Text = error->ToString();
        Windows::UI::Xaml::Window::Current->Content = errorText;
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
            const auto frame = ref new Windows::UI::Xaml::Controls::Frame();
            Windows::UI::Xaml::Window::Current->Content = frame;
            Windows::UI::Xaml::Window::Current->Activate();
            frame->Dispatcher->RunAsync(
                Windows::UI::Core::CoreDispatcherPriority::Normal,
                ref new Windows::UI::Core::DispatchedHandler([frame]() {
                    try
                    {
                        const auto settings = Microsoft::Terminal::Settings::Model::CascadiaSettings::LoadDefaults();
                        const auto viewModel = ref new Microsoft::Terminal::Settings::Editor::AIAgentsViewModel(settings->GlobalSettings);
                        const auto args = ref new Microsoft::Terminal::Settings::Editor::NavigateToPageArgs(viewModel, nullptr, L"");
                        if (!frame->Navigate(
                                Windows::UI::Xaml::Interop::TypeName(Microsoft::Terminal::Settings::Editor::AIAgents::typeid),
                                args))
                        {
                            throw ref new Platform::FailureException(L"Settings Agents navigation failed.");
                        }
                    }
                    catch (Platform::Exception ^ error)
                    {
                        _ShowAccessibilityStartupError(error);
                    }
                }));
            return;
        }

        Windows::UI::Xaml::Window::Current->Activate();
        Microsoft::VisualStudio::TestPlatform::TestExecutor::WinRTCore::UnitTestClient::Run(e->Arguments);
    }
}
