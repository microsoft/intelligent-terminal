// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include <fstream>
#include <winrt/Windows.ApplicationModel.h>
#include <winrt/Windows.Data.Xml.Dom.h>
#include <winrt/Windows.Storage.h>
#include <winrt/Windows.UI.Xaml.Shapes.h>
#include "../TerminalApp/TabHeaderControl.h"
#include "../TerminalApp/TabStrip.h"
#include "../TerminalApp/TerminalTabStatus.h"
#include <winrt/Windows.UI.Xaml.Automation.h>

using namespace winrt;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Controls;
namespace MUX = winrt::Microsoft::UI::Xaml::Controls;

extern "C" HRESULT __stdcall InitializeSidebarVisualFixture(wchar_t* errorMessage, uint32_t errorCapacity) noexcept
{
    const wchar_t* stage = L"loading production resources";
    try
    {
        const auto resourcesPath = std::filesystem::path{
            winrt::Windows::ApplicationModel::Package::Current().InstalledLocation().Path().c_str()
        } / L"TerminalApp" / L"App.xaml";
        std::ifstream resourcesFile{ resourcesPath };
        if (!resourcesFile)
        {
            return HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND);
        }
        const std::string resourcesBytes{ std::istreambuf_iterator<char>{ resourcesFile }, {} };
        auto resourcesXml = std::string_view{ resourcesBytes };
        if (resourcesXml.starts_with("\xEF\xBB\xBF"))
        {
            resourcesXml.remove_prefix(3);
        }
        winrt::Windows::Data::Xml::Dom::XmlDocument document;
        document.LoadXml(to_hstring(resourcesXml));
        const auto dictionary = document.SelectSingleNode(
                                            L"/*[local-name()='Application']/*[local-name()='Application.Resources']/*[local-name()='ResourceDictionary']")
                                    .as<winrt::Windows::Data::Xml::Dom::XmlElement>();
        dictionary.SetAttribute(L"xmlns", L"http://schemas.microsoft.com/winfx/2006/xaml/presentation");
        dictionary.SetAttribute(L"xmlns:x", L"http://schemas.microsoft.com/winfx/2006/xaml");
        dictionary.SetAttribute(L"xmlns:primitives", L"using:Microsoft.UI.Xaml.Controls.Primitives");
        stage = L"instantiating production resource dictionary";
        Application::Current().Resources().MergedDictionaries().Append(
            Markup::XamlReader::Load(dictionary.GetXml()).as<ResourceDictionary>());

        stage = L"creating fixture root";
        const auto root = Markup::XamlReader::Load(
                              LR"(<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
                  Background="{ThemeResource ApplicationPageBackgroundThemeBrush}">
                <Grid.RowDefinitions>
                    <RowDefinition Height="Auto"/>
                    <RowDefinition Height="*"/>
                </Grid.RowDefinitions>
            </Grid>)")
                              .as<Grid>();
        root.RequestedTheme(ElementTheme::Dark);
        Automation::AutomationProperties::SetAutomationId(root, L"SidebarVisualFixture");

        StackPanel toolbar;
        toolbar.Orientation(Orientation::Horizontal);
        toolbar.Spacing(16);
        toolbar.Margin({ 16, 8, 16, 8 });
        root.Children().Append(toolbar);

        stage = L"creating production TabStrip";
        const winrt::TerminalApp::TabStrip strip;
        strip.Width(272);
        strip.HorizontalAlignment(HorizontalAlignment::Left);
        Grid::SetRow(strip, 1);
        root.Children().Append(strip);

        ToggleSwitch lightTheme;
        lightTheme.Header(box_value(L"Light theme"));
        Automation::AutomationProperties::SetAutomationId(lightTheme, L"FixtureLightTheme");
        lightTheme.Toggled([weakRoot = make_weak(root)](const winrt::Windows::Foundation::IInspectable& sender, const RoutedEventArgs&) {
            if (const auto host = weakRoot.get())
            {
                host.RequestedTheme(sender.as<ToggleSwitch>().IsOn() ? ElementTheme::Light : ElementTheme::Dark);
            }
        });
        toolbar.Children().Append(lightTheme);

        ToggleSwitch rtl;
        rtl.Header(box_value(L"Right to left"));
        Automation::AutomationProperties::SetAutomationId(rtl, L"FixtureRightToLeft");
        rtl.Toggled([weakStrip = make_weak(strip)](const winrt::Windows::Foundation::IInspectable& sender, const RoutedEventArgs&) {
            if (const auto sidebar = weakStrip.get())
            {
                sidebar.FlowDirection(sender.as<ToggleSwitch>().IsOn() ? FlowDirection::RightToLeft : FlowDirection::LeftToRight);
            }
        });
        toolbar.Children().Append(rtl);

        Slider width;
        width.Header(box_value(L"Sidebar width"));
        width.Minimum(180);
        width.Maximum(400);
        width.Value(272);
        width.Width(180);
        Automation::AutomationProperties::SetAutomationId(width, L"FixtureSidebarWidth");
        width.ValueChanged([weakStrip = make_weak(strip)](const winrt::Windows::Foundation::IInspectable&, const Primitives::RangeBaseValueChangedEventArgs& args) {
            if (const auto sidebar = weakStrip.get())
            {
                sidebar.Width(args.NewValue());
            }
        });
        toolbar.Children().Append(width);

        const auto addTab = [&](const wchar_t* title, const wchar_t* metadata, const wchar_t* icon) {
            MUX::TabViewItem tab;
            winrt::TerminalApp::TabHeaderControl header;
            header.Title(title);
            header.MetadataText(metadata);
            header.IsMetadataVisible(*metadata != L'\0');
            header.TabStatus(make<winrt::TerminalApp::implementation::TerminalTabStatus>());
            header.Presentation().AutomationName(title);
            tab.Header(header);
            strip.TabItems().Append(tab);
            strip.SetTabPresentation(tab, title, icon);
            return tab;
        };
        stage = L"creating production tab headers";
        const auto shell = addTab(L"PowerShell", L"C:\\workspace\\sample", L"ms-appx:///taef.png");
        addTab(L"Settings", L"", L"");
        const auto group = addTab(L"GitHub Copilot", L"", L"ms-appx:///AgentIcons/copilot.svg");
        auto panes = single_threaded_vector<winrt::TerminalApp::TabStripPaneItem>();
        for (uint32_t index = 0; index < 2; ++index)
        {
            const auto pane = make<winrt::TerminalApp::implementation::TabStripPaneItem>(
                group, index + 1, L"ms-appx:///AgentIcons/copilot.svg", L"GitHub Copilot", index == 0);
            pane.MetadataText(L"Idle\nC:\\workspace\\sample");
            pane.MetadataVisibility(Visibility::Visible);
            panes.Append(pane);
        }
        stage = L"populating pane group";
        strip.SetPaneItems(group, panes, true);
        const auto stripImpl = get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
        stripImpl->DisplayItemForTab(group).IsExpanded(true);
        strip.SelectedItem(shell);
        strip.HistoryActive(true);

        stage = L"populating history";
        const auto history = make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
        history.SessionId(L"sidebar-visual-fixture-history");
        history.Title(L"Review layout composition");
        history.Cwd(L"C:\\workspace\\sample");
        history.AgentId(L"copilot");
        history.ProviderDisplayName(L"GitHub Copilot");
        history.Status(L"Ended");
        history.IsHistorical(true);
        history.Subtitle(L"1h");
        stripImpl->CommitHistorySnapshot({ history }, true);

        strip.SearchChanged([weakStrip = make_weak(strip)](const auto&, const auto&) {
            if (const auto sidebar = weakStrip.get())
            {
                const auto query = sidebar.SearchQuery();
                for (const auto& item : sidebar.TabItems())
                {
                    const auto tab = item.as<MUX::TabViewItem>();
                    const auto title = tab.Header().as<winrt::TerminalApp::TabHeaderControl>().Title();
                    sidebar.SetTabItemVisibility(tab, query.empty() || std::wstring_view{ title }.find(query) != std::wstring_view::npos);
                }
            }
        });

        Window::Current().Content(root);
        return S_OK;
    }
    catch (const winrt::hresult_error& error)
    {
        _snwprintf_s(errorMessage, errorCapacity, _TRUNCATE, L"%s: %s", stage, error.message().c_str());
        return error.code();
    }
    CATCH_RETURN()
}
