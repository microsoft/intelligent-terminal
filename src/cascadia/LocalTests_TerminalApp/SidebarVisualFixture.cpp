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
#include "../TerminalApp/Tab.h"
#include "../TerminalApp/TabRowControl.h"
#include "../TerminalApp/BasicPaneEvents.h"
#include "../WinRTUtils/inc/Utils.h"
#include <winrt/Windows.UI.Xaml.Automation.h>

using namespace winrt;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Controls;
namespace MUX = winrt::Microsoft::UI::Xaml::Controls;
namespace Model = winrt::Microsoft::Terminal::Settings::Model;

namespace
{
    struct VisualFixturePaneContent :
        implements<VisualFixturePaneContent, winrt::TerminalApp::IPaneContent>,
        winrt::TerminalApp::implementation::BasicPaneEvents
    {
        explicit VisualFixturePaneContent(hstring title) :
            _title{ std::move(title) }
        {
        }

        FrameworkElement GetRoot() { return _root; }
        void UpdateSettings(const Model::CascadiaSettings&) {}
        winrt::Windows::Foundation::Size MinimumSize() { return { 1, 1 }; }
        hstring Title() { return _title; }
        uint64_t TaskbarState() { return 0; }
        uint64_t TaskbarProgress() { return 0; }
        bool ReadOnly() { return false; }
        hstring Icon() { return L""; }
        winrt::Windows::Foundation::IReference<winrt::Windows::UI::Color> TabColor() { return nullptr; }
        Media::Brush BackgroundBrush() { return _background; }
        Model::INewContentArgs GetNewTerminalArgs(winrt::TerminalApp::BuildStartupKind) { return Model::BaseContentArgs{ L"settings" }; }
        void Focus(FocusState) {}
        void Close() {}

    private:
        Grid _root;
        hstring _title;
        Media::SolidColorBrush _background{ winrt::Windows::UI::Colors::Black() };
    };
}

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
        stage = L"parsing production resources";
        document.LoadXml(to_hstring(resourcesXml));
        stage = L"selecting production resource dictionary";
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
        Automation::AutomationProperties::SetAutomationId(strip, L"FixtureSidebar");
        Automation::AutomationProperties::SetItemStatus(strip, L"PaneRequest:None");
        strip.Width(272);
        strip.HorizontalAlignment(HorizontalAlignment::Left);
        const winrt::TerminalApp::TabRowControl chrome;
        const auto chromeImpl = get_self<winrt::TerminalApp::implementation::TabRowControl>(chrome);
        chromeImpl->IsVerticalLayout(true);
        chromeImpl->SetVerticalRailState(true, false, 272);
        strip.TopChromeContent(chromeImpl->VerticalTitleBarContent());
        Grid::SetRow(strip, 1);
        root.Children().Append(strip);

        ComboBox themePreset;
        themePreset.Header(box_value(L"Application theme preset"));
        Automation::AutomationProperties::SetAutomationId(themePreset, L"FixtureThemePreset");
        for (const auto& [label, name] : {
                 std::pair{ L"Dark", L"dark" },
                 std::pair{ L"Dark (legacy)", L"legacyDark" },
                 std::pair{ L"Light", L"light" },
                 std::pair{ L"Light (legacy)", L"legacyLight" } })
        {
            ComboBoxItem item;
            item.Content(box_value(hstring{ label }));
            item.Tag(box_value(hstring{ name }));
            Automation::AutomationProperties::SetAutomationId(item, hstring{ L"FixturePreset-" } + name);
            themePreset.Items().Append(item);
        }
        themePreset.SelectedIndex(0);
        toolbar.Children().Append(themePreset);

        ToggleSwitch activePalette;
        activePalette.Header(box_value(L"Active window palette"));
        activePalette.IsOn(true);
        Automation::AutomationProperties::SetAutomationId(activePalette, L"FixtureActivePalette");
        toolbar.Children().Append(activePalette);

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
        width.ValueChanged([weakStrip = make_weak(strip), chrome](const winrt::Windows::Foundation::IInspectable&, const Primitives::RangeBaseValueChangedEventArgs& args) {
            if (const auto sidebar = weakStrip.get())
            {
                const auto effectiveWidth = sidebar.IsRailCollapsed() ? 40.0 : args.NewValue();
                sidebar.Width(effectiveWidth);
                get_self<winrt::TerminalApp::implementation::TabRowControl>(chrome)->SetVerticalRailState(
                    true, sidebar.IsRailCollapsed(), effectiveWidth);
            }
        });
        toolbar.Children().Append(width);
        chromeImpl->RailCollapseRequested([weakStrip = make_weak(strip), weakChrome = make_weak(chrome), weakWidth = make_weak(width)](const auto&, const auto&) {
            if (const auto sidebar = weakStrip.get())
            {
                if (const auto row = weakChrome.get())
                {
                    if (const auto slider = weakWidth.get())
                    {
                        const auto collapsed = !sidebar.IsRailCollapsed();
                        const auto effectiveWidth = collapsed ? 40.0 : slider.Value();
                        sidebar.IsRailCollapsed(collapsed);
                        sidebar.Width(effectiveWidth);
                        get_self<winrt::TerminalApp::implementation::TabRowControl>(row)->SetVerticalRailState(true, collapsed, effectiveWidth);
                    }
                }
            }
        });

        std::vector<com_ptr<winrt::TerminalApp::implementation::Tab>> ownedTabs;
        const auto addTab = [&](const wchar_t* title, const wchar_t* metadata, const wchar_t* icon) {
            const auto content = make<VisualFixturePaneContent>(title);
            const auto owner = make_self<winrt::TerminalApp::implementation::Tab>(std::make_shared<Pane>(content));
            const auto tab = owner->TabViewItem();
            const auto header = tab.Header().as<winrt::TerminalApp::TabHeaderControl>();
            header.Title(title);
            header.MetadataText(metadata);
            header.IsMetadataVisible(*metadata != L'\0');
            header.TabStatus(make<winrt::TerminalApp::implementation::TerminalTabStatus>());
            header.Presentation().AutomationName(title);
            tab.Header(header);
            strip.TabItems().Append(tab);
            strip.SetTabPresentation(tab, title, icon);
            owner->TabColorChanged([weakStrip = make_weak(strip), weakTab = make_weak(tab)]() {
                if (const auto sidebar = weakStrip.get())
                {
                    if (const auto item = weakTab.get())
                    {
                        get_self<winrt::TerminalApp::implementation::TabStrip>(sidebar)->RefreshTabColor(item);
                    }
                }
            });
            ownedTabs.emplace_back(owner);
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

        strip.PaneActivationRequested([weakStrip = make_weak(strip)](const auto&, const winrt::TerminalApp::TabStripPaneEventArgs& args) {
            if (const auto sidebar = weakStrip.get())
            {
                Automation::AutomationProperties::SetItemStatus(sidebar, L"PaneActivationRequested:" + to_hstring(args.ContentId()));
                const auto display = get_self<winrt::TerminalApp::implementation::TabStrip>(sidebar)->DisplayItemForTab(args.Tab());
                const auto panes = single_threaded_vector<winrt::TerminalApp::TabStripPaneItem>();
                for (const auto& pane : display.PaneItems())
                {
                    pane.IsActive(pane.ContentId() == args.ContentId());
                    panes.Append(pane);
                }
                sidebar.SelectedItem(args.Tab());
                sidebar.SetPaneItems(args.Tab(), panes, true);
            }
        });
        strip.PaneCloseRequested([weakStrip = make_weak(strip)](const auto&, const winrt::TerminalApp::TabStripPaneEventArgs& args) {
            if (const auto sidebar = weakStrip.get())
            {
                Automation::AutomationProperties::SetItemStatus(sidebar, L"PaneCloseRequested:" + to_hstring(args.ContentId()));
            }
        });

        stage = L"loading built-in theme presets";
        const auto settings = Model::CascadiaSettings::LoadDefaults();
        const auto applyPreset = [weakRoot = make_weak(root), weakStrip = make_weak(strip), weakPreset = make_weak(themePreset), weakActive = make_weak(activePalette), themes = settings.GlobalSettings().Themes(), ownedTabs]() {
            const auto host = weakRoot.get();
            const auto sidebar = weakStrip.get();
            const auto picker = weakPreset.get();
            const auto active = weakActive.get();
            if (!host || !sidebar || !picker || !active)
            {
                return;
            }
            const auto name = unbox_value<hstring>(picker.SelectedItem().as<ComboBoxItem>().Tag());
            const auto theme = themes.Lookup(name);
            host.RequestedTheme(theme.RequestedTheme());
            const auto resources = Application::Current().Resources();
            Media::Brush background = ThemeLookup(resources, theme.RequestedTheme(), box_value(L"TabViewBackground")).as<Media::Brush>();
            if (const auto row = theme.TabRow())
            {
                if (const auto color = active.IsOn() ? row.Background() : row.UnfocusedBackground())
                {
                    background = color.Evaluate(resources, nullptr, true);
                }
            }
            sidebar.Background(background);
            const til::color rowColor{ Model::ThemeColor::ColorFromBrush(background) };
            for (const auto& owner : ownedTabs)
            {
                owner->ThemeColor(theme.Tab() ? theme.Tab().Background() : nullptr,
                                  theme.Tab() ? theme.Tab().UnfocusedBackground() : nullptr,
                                  rowColor);
            }
        };
        themePreset.SelectionChanged([applyPreset](const auto&, const SelectionChangedEventArgs&) { applyPreset(); });
        activePalette.Toggled([applyPreset](const auto&, const RoutedEventArgs&) { applyPreset(); });
        root.Loaded([applyPreset](const winrt::Windows::Foundation::IInspectable& sender, const RoutedEventArgs&) {
            sender.as<FrameworkElement>().Dispatcher().RunAsync(
                winrt::Windows::UI::Core::CoreDispatcherPriority::Normal,
                [applyPreset]() { applyPreset(); });
        });

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
        stage = L"applying initial theme preset";
        applyPreset();
        return S_OK;
    }
    catch (const winrt::hresult_error& error)
    {
        _snwprintf_s(errorMessage, errorCapacity, _TRUNCATE, L"%s: %s", stage, error.message().c_str());
        return error.code();
    }
    CATCH_RETURN()
}
