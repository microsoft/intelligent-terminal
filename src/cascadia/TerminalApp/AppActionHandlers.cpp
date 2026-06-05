// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "App.h"

#include "TerminalPage.h"
#include "AgentPaneContent.h"
#include "AgentPaneLog.h"
#include "ScratchpadContent.h"
#include "../inc/ShellIntegration.h"
#include "../WinRTUtils/inc/WtExeUtils.h"
#include "../../types/inc/utils.hpp"
#include "../TerminalSettingsAppAdapterLib/TerminalSettings.h"
#include "Utils.h"
#include <json/json.h>

using namespace winrt::Windows::ApplicationModel::DataTransfer;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Text;
using namespace winrt::Windows::UI::Core;
using namespace winrt::Windows::Foundation::Collections;
using namespace winrt::Windows::System;
using namespace winrt::Microsoft::Terminal;
using namespace winrt::Microsoft::Terminal::Settings::Model;
using namespace winrt::Microsoft::Terminal::Control;
using namespace winrt::Microsoft::Terminal::TerminalConnection;
using namespace ::TerminalApp;

namespace winrt
{
    namespace MUX = Microsoft::UI::Xaml;
    using IInspectable = Windows::Foundation::IInspectable;
}

namespace winrt::TerminalApp::implementation
{
    TermControl TerminalPage::_senderOrActiveControl(const IInspectable& sender)
    {
        if (sender)
        {
            if (auto arg{ sender.try_as<TermControl>() })
            {
                return arg;
            }
        }
        return _GetActiveControl();
    }
    winrt::com_ptr<Tab> TerminalPage::_senderOrFocusedTab(const IInspectable& sender)
    {
        if (sender)
        {
            if (auto tab = sender.try_as<TerminalApp::Tab>())
            {
                return _GetTabImpl(tab);
            }
        }
        return _GetFocusedTabImpl();
    }

    void TerminalPage::_HandleOpenNewTabDropdown(const IInspectable& /*sender*/,
                                                 const ActionEventArgs& args)
    {
        _OpenNewTabDropdown();
        args.Handled(true);
    }

    void TerminalPage::_HandleDuplicateTab(const IInspectable& /*sender*/,
                                           const ActionEventArgs& args)
    {
        _DuplicateFocusedTab();
        args.Handled(true);
    }

    void TerminalPage::_HandleCloseTab(const IInspectable& /*sender*/,
                                       const ActionEventArgs& args)
    {
        if (const auto realArgs = args.ActionArgs().try_as<CloseTabArgs>())
        {
            uint32_t index;
            if (realArgs.Index())
            {
                index = realArgs.Index().Value();
            }
            else if (auto focusedTabIndex = _GetFocusedTabIndex())
            {
                index = *focusedTabIndex;
            }
            else
            {
                args.Handled(false);
                return;
            }

            _CloseTabAtIndex(index);
            args.Handled(true);
        }
    }

    void TerminalPage::_HandleClosePane(const IInspectable& /*sender*/,
                                        const ActionEventArgs& args)
    {
        _CloseFocusedPane();
        args.Handled(true);
    }

    void TerminalPage::_HandleRestoreLastClosed(const IInspectable& /*sender*/,
                                                const ActionEventArgs& args)
    {
        if (_previouslyClosedPanesAndTabs.size() > 0)
        {
            const auto restoreActions = _previouslyClosedPanesAndTabs.back();
            for (const auto& action : restoreActions)
            {
                _actionDispatch->DoAction(action);
            }
            _previouslyClosedPanesAndTabs.pop_back();

            args.Handled(true);
        }
    }

    void TerminalPage::_HandleCloseWindow(const IInspectable& /*sender*/,
                                          const ActionEventArgs& args)
    {
        CloseWindow();
        args.Handled(true);
    }

    void TerminalPage::_HandleQuit(const IInspectable& /*sender*/,
                                   const ActionEventArgs& args)
    {
        RequestQuit();
        args.Handled(true);
    }

    void TerminalPage::_HandleScrollUp(const IInspectable& /*sender*/,
                                       const ActionEventArgs& args)
    {
        const auto& realArgs = args.ActionArgs().try_as<ScrollUpArgs>();
        if (realArgs)
        {
            _Scroll(ScrollUp, realArgs.RowsToScroll());
            args.Handled(true);
        }
    }

    void TerminalPage::_HandleScrollDown(const IInspectable& /*sender*/,
                                         const ActionEventArgs& args)
    {
        const auto& realArgs = args.ActionArgs().try_as<ScrollDownArgs>();
        if (realArgs)
        {
            _Scroll(ScrollDown, realArgs.RowsToScroll());
            args.Handled(true);
        }
    }

    void TerminalPage::_HandleNextTab(const IInspectable& /*sender*/,
                                      const ActionEventArgs& args)
    {
        const auto& realArgs = args.ActionArgs().try_as<NextTabArgs>();
        if (realArgs)
        {
            _SelectNextTab(true, realArgs.SwitcherMode());
            args.Handled(true);
        }
    }

    void TerminalPage::_HandlePrevTab(const IInspectable& /*sender*/,
                                      const ActionEventArgs& args)
    {
        const auto& realArgs = args.ActionArgs().try_as<PrevTabArgs>();
        if (realArgs)
        {
            _SelectNextTab(false, realArgs.SwitcherMode());
            args.Handled(true);
        }
    }

    void TerminalPage::_HandleSendInput(const IInspectable& sender,
                                        const ActionEventArgs& args)
    {
        if (args == nullptr)
        {
            args.Handled(false);
        }
        else if (const auto& realArgs = args.ActionArgs().try_as<SendInputArgs>())
        {
            if (const auto termControl{ _senderOrActiveControl(sender) })
            {
                termControl.SendInput(realArgs.Input());
                args.Handled(true);
            }
        }
    }

    void TerminalPage::_HandleCloseOtherPanes(const IInspectable& sender,
                                              const ActionEventArgs& args)
    {
        if (const auto& activeTab{ _senderOrFocusedTab(sender) })
        {
            const auto activePane = activeTab->GetActivePane();
            if (activeTab->GetRootPane() != activePane)
            {
                _UnZoomIfNeeded();

                // Accumulate list of all unfocused leaf panes, ignore read-only panes
                std::vector<uint32_t> unfocusedPaneIds;
                const auto activePaneId = activePane->Id();
                activeTab->GetRootPane()->WalkTree([&](auto&& p) {
                    const auto id = p->Id();
                    if (id.has_value() && id != activePaneId && !p->ContainsReadOnly())
                    {
                        unfocusedPaneIds.push_back(id.value());
                    }
                });

                if (!empty(unfocusedPaneIds))
                {
                    // Start by removing the panes that were least recently added
                    sort(begin(unfocusedPaneIds), end(unfocusedPaneIds), std::less<uint32_t>());
                    _ClosePanes(activeTab->get_weak(), std::move(unfocusedPaneIds));
                    args.Handled(true);
                    return;
                }
            }
            args.Handled(false);
        }
    }

    void TerminalPage::_HandleMovePane(const IInspectable& /*sender*/,
                                       const ActionEventArgs& args)
    {
        if (args == nullptr)
        {
            args.Handled(false);
        }
        else if (const auto& realArgs = args.ActionArgs().try_as<MovePaneArgs>())
        {
            const auto moved = _MovePane(realArgs);
            args.Handled(moved);
        }
    }

    // * Helper to try and get a ProfileIndex out of a NewTerminalArgs out of a
    //   NewContentArgs. For the new tab and split pane action, we want to _not_
    //   handle the event if an invalid profile index was passed.
    //
    // Return value:
    // * True if the args are NewTerminalArgs, and the profile index was out of bounds.
    // * False otherwise.
    static bool _shouldBailForInvalidProfileIndex(const CascadiaSettings& settings, const INewContentArgs& args)
    {
        if (!args)
        {
            return false;
        }
        if (const auto& terminalArgs{ args.try_as<NewTerminalArgs>() })
        {
            if (const auto index = terminalArgs.ProfileIndex())
            {
                if (gsl::narrow<uint32_t>(index.Value()) >= settings.ActiveProfiles().Size())
                {
                    return true;
                }
            }
        }
        return false;
    }

    void TerminalPage::_HandleSplitPane(const IInspectable& sender,
                                        const ActionEventArgs& args)
    {
        if (args == nullptr)
        {
            args.Handled(false);
        }
        else if (const auto& realArgs = args.ActionArgs().try_as<SplitPaneArgs>())
        {
            if (_shouldBailForInvalidProfileIndex(_settings, realArgs.ContentArgs()))
            {
                args.Handled(false);
                return;
            }

            const auto& duplicateFromTab{ realArgs.SplitMode() == SplitType::Duplicate ? _GetFocusedTab() : nullptr };

            const auto& activeTab{ _senderOrFocusedTab(sender) };

            _SplitPane(activeTab,
                       realArgs.SplitDirection(),
                       // This is safe, we're already filtering so the value is (0, 1)
                       realArgs.SplitSize(),
                       _MakePane(realArgs.ContentArgs(), duplicateFromTab));
            args.Handled(true);
        }
    }

    void TerminalPage::_HandleToggleSplitOrientation(const IInspectable& /*sender*/,
                                                     const ActionEventArgs& args)
    {
        _ToggleSplitOrientation();
        args.Handled(true);
    }

    void TerminalPage::_HandleTogglePaneZoom(const IInspectable& sender,
                                             const ActionEventArgs& args)
    {
        if (const auto activeTab{ _senderOrFocusedTab(sender) })
        {
            // Don't do anything if there's only one pane. It's already zoomed.
            if (activeTab->GetLeafPaneCount() > 1)
            {
                // Togging the zoom on the tab will cause the tab to inform us of
                // the new root Content for this tab.
                activeTab->ToggleZoom();
            }
        }

        args.Handled(true);
    }

    void TerminalPage::_HandleTogglePaneVisibility(const IInspectable& sender,
                                                   const ActionEventArgs& args)
    {
        if (const auto activeTab{ _senderOrFocusedTab(sender) })
        {
            // Un-zoom first if needed, so the pane tree is fully visible
            // before we toggle visibility.
            if (activeTab->IsZoomed())
            {
                _tabContent.Children().Clear();
                activeTab->ExitZoom();
            }

            // Only toggle if there are multiple panes (can't hide the only pane).
            if (activeTab->GetLeafPaneCount() > 1 || activeTab->HasHiddenPane())
            {
                activeTab->TogglePaneVisibility();
            }
        }

        args.Handled(true);
    }

    void TerminalPage::_HandleTogglePaneReadOnly(const IInspectable& sender,
                                                 const ActionEventArgs& args)
    {
        if (const auto activeTab{ _senderOrFocusedTab(sender) })
        {
            activeTab->TogglePaneReadOnly();
        }

        args.Handled(true);
    }

    void TerminalPage::_HandleEnablePaneReadOnly(const IInspectable& sender,
                                                 const ActionEventArgs& args)
    {
        if (const auto activeTab{ _senderOrFocusedTab(sender) })
        {
            activeTab->SetPaneReadOnly(true);
        }

        args.Handled(true);
    }

    void TerminalPage::_HandleDisablePaneReadOnly(const IInspectable& sender,
                                                  const ActionEventArgs& args)
    {
        if (const auto activeTab{ _senderOrFocusedTab(sender) })
        {
            activeTab->SetPaneReadOnly(false);
        }

        args.Handled(true);
    }

    void TerminalPage::_HandleScrollUpPage(const IInspectable& /*sender*/,
                                           const ActionEventArgs& args)
    {
        _ScrollPage(ScrollUp);
        args.Handled(true);
    }

    void TerminalPage::_HandleScrollDownPage(const IInspectable& /*sender*/,
                                             const ActionEventArgs& args)
    {
        _ScrollPage(ScrollDown);
        args.Handled(true);
    }

    void TerminalPage::_HandleScrollToTop(const IInspectable& /*sender*/,
                                          const ActionEventArgs& args)
    {
        _ScrollToBufferEdge(ScrollUp);
        args.Handled(true);
    }

    void TerminalPage::_HandleScrollToBottom(const IInspectable& /*sender*/,
                                             const ActionEventArgs& args)
    {
        _ScrollToBufferEdge(ScrollDown);
        args.Handled(true);
    }

    void TerminalPage::_HandleScrollToMark(const IInspectable& /*sender*/,
                                           const ActionEventArgs& args)
    {
        if (const auto& realArgs = args.ActionArgs().try_as<ScrollToMarkArgs>())
        {
            _ApplyToActiveControls([&realArgs](auto& control) {
                control.ScrollToMark(realArgs.Direction());
            });
        }
        args.Handled(true);
    }
    void TerminalPage::_HandleAddMark(const IInspectable& /*sender*/,
                                      const ActionEventArgs& args)
    {
        if (const auto& realArgs = args.ActionArgs().try_as<AddMarkArgs>())
        {
            _ApplyToActiveControls([realArgs](auto& control) {
                Control::ScrollMark mark;
                if (realArgs.Color())
                {
                    mark.Color.Color = realArgs.Color().Value();
                    mark.Color.HasValue = true;
                }
                else
                {
                    mark.Color.HasValue = false;
                }
                control.AddMark(mark);
            });
        }
        args.Handled(true);
    }
    void TerminalPage::_HandleClearMark(const IInspectable& /*sender*/,
                                        const ActionEventArgs& args)
    {
        _ApplyToActiveControls([](auto& control) {
            control.ClearMark();
        });
        args.Handled(true);
    }
    void TerminalPage::_HandleClearAllMarks(const IInspectable& /*sender*/,
                                            const ActionEventArgs& args)
    {
        _ApplyToActiveControls([](auto& control) {
            control.ClearAllMarks();
        });
        args.Handled(true);
    }

    void TerminalPage::_HandleFindMatch(const IInspectable& /*sender*/,
                                        const ActionEventArgs& args)
    {
        if (const auto& realArgs = args.ActionArgs().try_as<FindMatchArgs>())
        {
            if (const auto& control{ _GetActiveControl() })
            {
                control.SearchMatch(realArgs.Direction() == FindMatchDirection::Next);
                args.Handled(true);
            }
        }
    }
    void TerminalPage::_HandleOpenSettings(const IInspectable& /*sender*/,
                                           const ActionEventArgs& args)
    {
        if (const auto& realArgs = args.ActionArgs().try_as<OpenSettingsArgs>())
        {
            _LaunchSettings(realArgs.Target());
            args.Handled(true);
        }
    }

    void TerminalPage::_HandlePasteText(const IInspectable& /*sender*/,
                                        const ActionEventArgs& args)
    {
        _PasteText();
        args.Handled(true);
    }

    void TerminalPage::_HandleNewTab(const IInspectable& /*sender*/,
                                     const ActionEventArgs& args)
    {
        if (args == nullptr)
        {
            LOG_IF_FAILED(_OpenNewTab(nullptr));
            args.Handled(true);
        }
        else if (const auto& realArgs = args.ActionArgs().try_as<NewTabArgs>())
        {
            if (_shouldBailForInvalidProfileIndex(_settings, realArgs.ContentArgs()))
            {
                args.Handled(false);
                return;
            }

            LOG_IF_FAILED(_OpenNewTab(realArgs.ContentArgs()));
            args.Handled(true);
        }
    }

    void TerminalPage::_HandleSwitchToTab(const IInspectable& /*sender*/,
                                          const ActionEventArgs& args)
    {
        if (const auto& realArgs = args.ActionArgs().try_as<SwitchToTabArgs>())
        {
            _SelectTab({ realArgs.TabIndex() });
            args.Handled(true);
        }
    }

    void TerminalPage::_HandleResizePane(const IInspectable& /*sender*/,
                                         const ActionEventArgs& args)
    {
        if (const auto& realArgs = args.ActionArgs().try_as<ResizePaneArgs>())
        {
            if (realArgs.ResizeDirection() == ResizeDirection::None)
            {
                // Do nothing
                args.Handled(false);
            }
            else
            {
                const auto resizeSucceeded = _ResizePane(realArgs.ResizeDirection());
                args.Handled(resizeSucceeded);
            }
        }
    }

    void TerminalPage::_HandleMoveFocus(const IInspectable& /*sender*/,
                                        const ActionEventArgs& args)
    {
        if (const auto& realArgs = args.ActionArgs().try_as<MoveFocusArgs>())
        {
            if (realArgs.FocusDirection() == FocusDirection::None)
            {
                // Do nothing
                args.Handled(false);
            }
            else
            {
                // Mark as handled only when the move succeeded (e.g. when there
                // is a pane to move to); otherwise, mark as unhandled so the
                // keychord can propagate to the terminal (GH#6129)
                const auto moveSucceeded = _MoveFocus(realArgs.FocusDirection());
                args.Handled(moveSucceeded);
            }
        }
    }

    void TerminalPage::_HandleSwapPane(const IInspectable& /*sender*/,
                                       const ActionEventArgs& args)
    {
        if (const auto& realArgs = args.ActionArgs().try_as<SwapPaneArgs>())
        {
            if (realArgs.Direction() == FocusDirection::None)
            {
                // Do nothing
                args.Handled(false);
            }
            else
            {
                auto swapped = _SwapPane(realArgs.Direction());
                args.Handled(swapped);
            }
        }
    }

    void TerminalPage::_HandleCopyText(const IInspectable& /*sender*/,
                                       const ActionEventArgs& args)
    {
        if (const auto& realArgs = args.ActionArgs().try_as<CopyTextArgs>())
        {
            const auto copyFormatting = realArgs.CopyFormatting();
            const auto format = copyFormatting ? copyFormatting.Value() : _settings.GlobalSettings().CopyFormatting();
            const auto handled = _CopyText(realArgs.DismissSelection(), realArgs.SingleLine(), realArgs.WithControlSequences(), format);
            args.Handled(handled);
        }
    }

    void TerminalPage::_HandleAdjustFontSize(const IInspectable& /*sender*/,
                                             const ActionEventArgs& args)
    {
        if (const auto& realArgs = args.ActionArgs().try_as<AdjustFontSizeArgs>())
        {
            const auto res = _ApplyToActiveControls([&](auto& control) {
                control.AdjustFontSize(realArgs.Delta());
            });
            args.Handled(res);
        }
    }

    void TerminalPage::_HandleFind(const IInspectable& sender,
                                   const ActionEventArgs& args)
    {
        if (const auto activeTab{ _senderOrFocusedTab(sender) })
        {
            _SetFocusedTab(*activeTab);
            _Find(*activeTab);
        }
        args.Handled(true);
    }

    void TerminalPage::_HandleResetFontSize(const IInspectable& /*sender*/,
                                            const ActionEventArgs& args)
    {
        const auto res = _ApplyToActiveControls([](auto& control) {
            control.ResetFontSize();
        });
        args.Handled(res);
    }

    void TerminalPage::_HandleToggleShaderEffects(const IInspectable& /*sender*/,
                                                  const ActionEventArgs& args)
    {
        const auto res = _ApplyToActiveControls([](auto& control) {
            control.ToggleShaderEffects();
        });
        args.Handled(res);
    }

    void TerminalPage::_HandleToggleFocusMode(const IInspectable& /*sender*/,
                                              const ActionEventArgs& args)
    {
        ToggleFocusMode();
        args.Handled(true);
    }

    void TerminalPage::_HandleSetFocusMode(const IInspectable& /*sender*/,
                                           const ActionEventArgs& args)
    {
        if (const auto& realArgs = args.ActionArgs().try_as<SetFocusModeArgs>())
        {
            SetFocusMode(realArgs.IsFocusMode());
            args.Handled(true);
        }
    }

    void TerminalPage::_HandleToggleFullscreen(const IInspectable& /*sender*/,
                                               const ActionEventArgs& args)
    {
        ToggleFullscreen();
        args.Handled(true);
    }

    void TerminalPage::_HandleSetFullScreen(const IInspectable& /*sender*/,
                                            const ActionEventArgs& args)
    {
        if (const auto& realArgs = args.ActionArgs().try_as<SetFullScreenArgs>())
        {
            SetFullscreen(realArgs.IsFullScreen());
            args.Handled(true);
        }
    }

    void TerminalPage::_HandleSetMaximized(const IInspectable& /*sender*/,
                                           const ActionEventArgs& args)
    {
        if (const auto& realArgs = args.ActionArgs().try_as<SetMaximizedArgs>())
        {
            RequestSetMaximized(realArgs.IsMaximized());
            args.Handled(true);
        }
    }

    void TerminalPage::_HandleToggleAlwaysOnTop(const IInspectable& /*sender*/,
                                                const ActionEventArgs& args)
    {
        ToggleAlwaysOnTop();
        args.Handled(true);
    }

    void TerminalPage::_HandleToggleCommandPalette(const IInspectable& /*sender*/,
                                                   const ActionEventArgs& args)
    {
        if (const auto& realArgs = args.ActionArgs().try_as<ToggleCommandPaletteArgs>())
        {
            const auto p = LoadCommandPalette();
            const auto v = p.Visibility() == Visibility::Visible ? Visibility::Collapsed : Visibility::Visible;
            p.EnableCommandPaletteMode(realArgs.LaunchMode());
            p.Visibility(v);
            args.Handled(true);
        }
    }

    void TerminalPage::_HandleSetColorScheme(const IInspectable& /*sender*/,
                                             const ActionEventArgs& args)
    {
        args.Handled(false);
        if (const auto& realArgs = args.ActionArgs().try_as<SetColorSchemeArgs>())
        {
            if (const auto scheme = _settings.GlobalSettings().ColorSchemes().TryLookup(realArgs.SchemeName()))
            {
                auto temporarySettings{ winrt::make_self<Settings::TerminalSettings>() };
                temporarySettings->ApplyColorScheme(scheme);
                const auto res = _ApplyToActiveControls([&](auto& control) {
                    control.SetOverrideColorScheme(temporarySettings.try_as<winrt::Microsoft::Terminal::Core::ICoreScheme>());
                });
                args.Handled(res);
            }
        }
    }

    void TerminalPage::_HandleSetTabColor(const IInspectable& sender,
                                          const ActionEventArgs& args)
    {
        Windows::Foundation::IReference<Windows::UI::Color> tabColor;

        if (const auto& realArgs = args.ActionArgs().try_as<SetTabColorArgs>())
        {
            tabColor = realArgs.TabColor();
        }

        if (const auto activeTab{ _senderOrFocusedTab(sender) })
        {
            if (tabColor)
            {
                activeTab->SetRuntimeTabColor(tabColor.Value());
            }
            else
            {
                activeTab->ResetRuntimeTabColor();
            }
        }
        args.Handled(true);
    }

    void TerminalPage::_HandleOpenTabColorPicker(const IInspectable& sender,
                                                 const ActionEventArgs& args)
    {
        if (const auto activeTab{ _senderOrFocusedTab(sender) })
        {
            if (!_tabColorPicker)
            {
                _tabColorPicker = winrt::make<ColorPickupFlyout>();
            }

            activeTab->AttachColorPicker(_tabColorPicker);
        }
        args.Handled(true);
    }

    void TerminalPage::_HandleRenameTab(const IInspectable& sender,
                                        const ActionEventArgs& args)
    {
        std::optional<winrt::hstring> title;

        if (const auto& realArgs = args.ActionArgs().try_as<RenameTabArgs>())
        {
            title = realArgs.Title();
        }

        if (const auto activeTab{ _senderOrFocusedTab(sender) })
        {
            if (title.has_value())
            {
                activeTab->SetTabText(title.value());
            }
            else
            {
                activeTab->ResetTabText();
            }
        }
        args.Handled(true);
    }

    void TerminalPage::_HandleOpenTabRenamer(const IInspectable& sender,
                                             const ActionEventArgs& args)
    {
        if (const auto activeTab{ _senderOrFocusedTab(sender) })
        {
            activeTab->ActivateTabRenamer();
        }
        args.Handled(true);
    }

    void TerminalPage::_HandleExecuteCommandline(const IInspectable& /*sender*/,
                                                 const ActionEventArgs& actionArgs)
    {
        if (const auto& realArgs = actionArgs.ActionArgs().try_as<ExecuteCommandlineArgs>())
        {
            auto actions = ConvertExecuteCommandlineToActions(realArgs);
            if (!actions.empty())
            {
                actionArgs.Handled(true);
                ProcessStartupActions(std::move(actions));
            }
        }
    }

    void TerminalPage::_HandleCloseOtherTabs(const IInspectable& /*sender*/,
                                             const ActionEventArgs& actionArgs)
    {
        if (const auto& realArgs = actionArgs.ActionArgs().try_as<CloseOtherTabsArgs>())
        {
            uint32_t index;
            if (realArgs.Index())
            {
                index = realArgs.Index().Value();
            }
            else if (auto focusedTabIndex = _GetFocusedTabIndex())
            {
                index = *focusedTabIndex;
            }
            else
            {
                // Do nothing
                actionArgs.Handled(false);
                return;
            }

            // Since _RemoveTabs is asynchronous, create a snapshot of the  tabs we want to remove
            std::vector<winrt::TerminalApp::Tab> tabsToRemove;
            if (index > 0)
            {
                std::copy(begin(_tabs), begin(_tabs) + index, std::back_inserter(tabsToRemove));
            }

            if (index + 1 < _tabs.Size())
            {
                std::copy(begin(_tabs) + index + 1, end(_tabs), std::back_inserter(tabsToRemove));
            }

            _RemoveTabs(tabsToRemove);

            actionArgs.Handled(!tabsToRemove.empty());
        }
    }

    void TerminalPage::_HandleCloseTabsAfter(const IInspectable& /*sender*/,
                                             const ActionEventArgs& actionArgs)
    {
        if (const auto& realArgs = actionArgs.ActionArgs().try_as<CloseTabsAfterArgs>())
        {
            uint32_t index;
            if (realArgs.Index())
            {
                index = realArgs.Index().Value();
            }
            else if (auto focusedTabIndex = _GetFocusedTabIndex())
            {
                index = *focusedTabIndex;
            }
            else
            {
                // Do nothing
                actionArgs.Handled(false);
                return;
            }

            // Since _RemoveTabs is asynchronous, create a snapshot of the  tabs we want to remove
            std::vector<winrt::TerminalApp::Tab> tabsToRemove;
            std::copy(begin(_tabs) + index + 1, end(_tabs), std::back_inserter(tabsToRemove));
            _RemoveTabs(tabsToRemove);

            // TODO:GH#7182 For whatever reason, if you run this action
            // when the tab that's currently focused is _before_ the `index`
            // param, then the tabs will expand to fill the entire width of the
            // tab row, until you mouse over them. Probably has something to do
            // with tabs not resizing down until there's a mouse exit event.

            actionArgs.Handled(!tabsToRemove.empty());
        }
    }

    void TerminalPage::_HandleTabSearch(const IInspectable& /*sender*/,
                                        const ActionEventArgs& args)
    {
        const auto p = LoadCommandPalette();
        p.SetTabs(_tabs, _mruTabs);
        p.EnableTabSearchMode();
        p.Visibility(Visibility::Visible);

        args.Handled(true);
    }

    void TerminalPage::_HandleMoveTab(const IInspectable& sender,
                                      const ActionEventArgs& actionArgs)
    {
        if (const auto& realArgs = actionArgs.ActionArgs().try_as<MoveTabArgs>())
        {
            const auto moved = _MoveTab(_senderOrFocusedTab(sender), realArgs);
            actionArgs.Handled(moved);
        }
    }

    void TerminalPage::_HandleBreakIntoDebugger(const IInspectable& /*sender*/,
                                                const ActionEventArgs& actionArgs)
    {
        if (_settings.GlobalSettings().DebugFeaturesEnabled())
        {
            actionArgs.Handled(true);
            DebugBreak();
        }
    }

    // Function Description:
    // - Helper to launch a new WT instance. It can either launch the instance
    //   elevated or unelevated.
    // - To launch elevated, it will as the shell to elevate the process for us.
    //   This might cause a UAC prompt. The elevation is performed on a
    //   background thread, as to not block the UI thread.
    // Arguments:
    // - newTerminalArgs: A NewTerminalArgs describing the terminal instance
    //   that should be spawned. The Profile should be filled in with the GUID
    //   of the profile we want to launch.
    // Return Value:
    // - <none>
    // Important: Don't take the param by reference, since we'll be doing work
    // on another thread.
    safe_void_coroutine TerminalPage::_OpenNewWindow(const INewContentArgs newContentArgs)
    {
        auto terminalArgs{ newContentArgs.try_as<NewTerminalArgs>() };

        // Do nothing for non-terminal panes.
        //
        // Theoretically, we could define a `IHasCommandline` interface, and
        // stick `ToCommandline` on that interface, for any kind of pane that
        // wants to be convertable to a wt commandline.
        //
        // Another idea we're thinking about is just `wt do {literal json for an
        // action}`, which might be less leaky
        if (terminalArgs == nullptr)
        {
            co_return;
        }

        // ShellExecuteExW may block, so do it on a background thread.
        //
        // NOTE: All remaining code of this function doesn't touch `this`, so we don't need weak/strong_ref.
        // NOTE NOTE: Don't touch `this` when you make changes here.
        co_await winrt::resume_background();

        // This will get us the correct exe for dev/preview/release. If you
        // don't stick this in a local, it'll get mangled by ShellExecute. I
        // have no idea why.
        const auto exePath{ GetWtExePath() };

        // Build the commandline to pass to wt for this set of NewTerminalArgs
        // `-w -1` will ensure a new window is created.
        const auto commandline = terminalArgs.ToCommandline();
        const auto cmdline = fmt::format(FMT_COMPILE(L"-w -1 new-tab {}"), commandline);

        // Build the args to ShellExecuteEx. We need to use ShellExecuteEx so we
        // can pass the SEE_MASK_NOASYNC flag. That flag allows us to safely
        // call this on the background thread, and have ShellExecute _not_ call
        // back to us on the main thread. Without this, if you close the
        // Terminal quickly after the UAC prompt, the elevated WT will never
        // actually spawn.
        SHELLEXECUTEINFOW seInfo{ 0 };
        seInfo.cbSize = sizeof(seInfo);
        seInfo.fMask = SEE_MASK_NOASYNC;
        // `open` will just run the executable normally.
        seInfo.lpVerb = L"open";
        seInfo.lpFile = exePath.c_str();
        seInfo.lpParameters = cmdline.c_str();
        seInfo.nShow = SW_SHOWNORMAL;
        LOG_IF_WIN32_BOOL_FALSE(ShellExecuteExW(&seInfo));

        co_return;
    }

    void TerminalPage::_HandleNewWindow(const IInspectable& /*sender*/,
                                        const ActionEventArgs& actionArgs)
    {
        INewContentArgs newContentArgs{ nullptr };
        // If the caller provided NewTerminalArgs, then try to use those
        if (actionArgs)
        {
            if (const auto& realArgs = actionArgs.ActionArgs().try_as<NewWindowArgs>())
            {
                newContentArgs = realArgs.ContentArgs();
            }
        }
        // Otherwise, if no NewTerminalArgs were provided, then just use a
        // default-constructed one. The default-constructed one implies that
        // nothing about the launch should be modified (just use the default
        // profile).
        if (!newContentArgs)
        {
            newContentArgs = NewTerminalArgs{};
        }

        if (const auto& terminalArgs{ newContentArgs.try_as<NewTerminalArgs>() })
        {
            const auto profile{ _settings.GetProfileForArgs(terminalArgs) };
            terminalArgs.Profile(::Microsoft::Console::Utils::GuidToString(profile.Guid()));
        }

        // Manually fill in the evaluated profile.
        _OpenNewWindow(newContentArgs);
        actionArgs.Handled(true);
    }

    // Method Description:
    // - Raise a IdentifyWindowsRequested event. This will bubble up to the
    //   AppLogic, to the AppHost, to the Peasant, to the Monarch, then get
    //   distributed down to _all_ the Peasants, as to display info about the
    //   window in _every_ Peasant window.
    // - This action is also buggy right now, because TeachingTips behave
    //   weird in XAML Islands. See microsoft-ui-xaml#4382
    // Arguments:
    // - <unused>
    // Return Value:
    // - <none>
    void TerminalPage::_HandleIdentifyWindows(const IInspectable& /*sender*/,
                                              const ActionEventArgs& args)
    {
        IdentifyWindowsRequested.raise(*this, nullptr);
        args.Handled(true);
    }

    // Method Description:
    // - Display the "Toast" with the name and ID of this window.
    // - Unlike _HandleIdentifyWindow**s**, this event just displays the window
    //   ID and name in the current window. It does not involve any bubbling
    //   up/down the page/logic/host/manager/peasant/monarch.
    // Arguments:
    // - <unused>
    // Return Value:
    // - <none>
    void TerminalPage::_HandleIdentifyWindow(const IInspectable& /*sender*/,
                                             const ActionEventArgs& args)
    {
        IdentifyWindow();
        args.Handled(true);
    }

    void TerminalPage::_HandleRenameWindow(const IInspectable& /*sender*/,
                                           const ActionEventArgs& args)
    {
        if (args)
        {
            if (const auto& realArgs = args.ActionArgs().try_as<RenameWindowArgs>())
            {
                const auto newName = realArgs.Name();
                const auto request = winrt::make_self<implementation::RenameWindowRequestedArgs>(newName);
                RenameWindowRequested.raise(*this, *request);
                args.Handled(true);
            }
        }
    }

    void TerminalPage::_HandleOpenWindowRenamer(const IInspectable& /*sender*/,
                                                const ActionEventArgs& args)
    {
        if (WindowRenamer() == nullptr)
        {
            // We need to use FindName to lazy-load this object
            if (auto tip{ FindName(L"WindowRenamer").try_as<MUX::Controls::TeachingTip>() })
            {
                tip.Closed({ get_weak(), &TerminalPage::_FocusActiveControl });
            }
        }

        _UpdateTeachingTipTheme(WindowRenamer().try_as<winrt::Windows::UI::Xaml::FrameworkElement>());

        // BODGY: GH#12021
        //
        // TeachingTip doesn't provide an Opened event.
        // (microsoft/microsoft-ui-xaml#1607). But we want to focus the renamer
        // text box when it's opened. We can't do that immediately, the TextBox
        // technically isn't in the visual tree yet. We have to wait for it to
        // get added some time after we call IsOpen. How do we do that reliably?
        // Usually, for this kind of thing, we'd just use a one-off
        // LayoutUpdated event, as a notification that the TextBox was added to
        // the tree. HOWEVER:
        //   * The _first_ time this is fired, when the box is _first_ opened,
        //     tossing focus doesn't work on the first LayoutUpdated. It does
        //     work on the second LayoutUpdated. Okay, so we'll wait for two
        //     LayoutUpdated events, and focus on the second.
        //   * On subsequent opens: We only ever get a single LayoutUpdated.
        //     Period. But, you can successfully focus it on that LayoutUpdated.
        //
        // So, we'll keep track of how many LayoutUpdated's we've _ever_ gotten.
        // If we've had at least 2, then we can focus the text box.
        //
        // We're also not using a ContentDialog for this, because in Xaml
        // Islands a text box in a ContentDialog won't receive _any_ keypresses.
        // Fun!
        // WindowRenamerTextBox().Focus(FocusState::Programmatic);
        _renamerLayoutUpdatedRevoker.revoke();
        _renamerLayoutUpdatedRevoker = WindowRenamerTextBox().LayoutUpdated(winrt::auto_revoke, [weakThis = get_weak()](auto&&, auto&&) {
            if (auto self{ weakThis.get() })
            {
                auto& count{ self->_renamerLayoutCount };

                // Don't just always increment this, we don't want to deal with overflow situations
                if (count < 2)
                {
                    count++;
                }

                if (count >= 2)
                {
                    self->_renamerLayoutUpdatedRevoker.revoke();
                    self->WindowRenamerTextBox().Focus(FocusState::Programmatic);
                }
            }
        });
        // Make sure to mark that enter was not pressed in the renamer quite
        // yet. More details in TerminalPage::_WindowRenamerKeyDown.
        _renamerPressedEnter = false;
        WindowRenamer().IsOpen(true);

        args.Handled(true);
    }

    void TerminalPage::_HandleDisplayWorkingDirectory(const IInspectable& /*sender*/,
                                                      const ActionEventArgs& args)
    {
        if (_settings.GlobalSettings().DebugFeaturesEnabled())
        {
            ShowTerminalWorkingDirectory();
            args.Handled(true);
        }
    }

    void TerminalPage::_HandleSearchForText(const IInspectable& /*sender*/,
                                            const ActionEventArgs& args)
    {
        if (const auto termControl{ _GetActiveControl() })
        {
            if (termControl.HasSelection())
            {
                std::wstring searchText{ termControl.SelectedText(true) };

                // make it compact by replacing consecutive whitespaces with a single space
                searchText = std::regex_replace(searchText, std::wregex(LR"(\s+)"), L" ");

                std::wstring queryUrl;
                if (args)
                {
                    if (const auto& realArgs = args.ActionArgs().try_as<SearchForTextArgs>())
                    {
                        queryUrl = std::wstring_view{ realArgs.QueryUrl() };
                    }
                }

                // use global default if query URL is unspecified
                if (queryUrl.empty())
                {
                    queryUrl = std::wstring_view{ _settings.GlobalSettings().SearchWebDefaultQueryUrl() };
                }

                constexpr std::wstring_view queryToken{ L"%s" };
                if (const auto pos{ queryUrl.find(queryToken) }; pos != std::wstring_view::npos)
                {
                    queryUrl.replace(pos, queryToken.length(), Windows::Foundation::Uri::EscapeComponent(searchText));
                }

                winrt::Microsoft::Terminal::Control::OpenHyperlinkEventArgs shortcut{ queryUrl };
                _OpenHyperlinkHandler(termControl, shortcut);
                args.Handled(true);
            }
        }
    }

    void TerminalPage::_HandleOpenCWD(const IInspectable& /*sender*/,
                                      const ActionEventArgs& args)
    {
        if (const auto& control{ _GetActiveControl() })
        {
            control.OpenCWD();
            args.Handled(true);
        }
    }

    void TerminalPage::_HandleGlobalSummon(const IInspectable& /*sender*/,
                                           const ActionEventArgs& args)
    {
        // Manually return false. These shouldn't ever get here, except for when
        // we fail to register for the global hotkey. In that case, returning
        // false here will let the underlying terminal still process the key, as
        // if it wasn't bound at all.
        args.Handled(false);
    }
    void TerminalPage::_HandleQuakeMode(const IInspectable& /*sender*/,
                                        const ActionEventArgs& args)
    {
        // Manually return false. These shouldn't ever get here, except for when
        // we fail to register for the global hotkey. In that case, returning
        // false here will let the underlying terminal still process the key, as
        // if it wasn't bound at all.
        args.Handled(false);
    }

    void TerminalPage::_HandleFocusPane(const IInspectable& /*sender*/,
                                        const ActionEventArgs& args)
    {
        if (args)
        {
            if (const auto& realArgs = args.ActionArgs().try_as<FocusPaneArgs>())
            {
                const auto paneId = realArgs.Id();

                // This action handler is not enlightened for _senderOrFocusedTab.
                // There's currently no way for an inactive tab to be the sender of a focusPane command.
                // If that ever changes, then we'll need to consider how this handler should behave.
                // Should it
                // * focus the tab that sent the command AND activate the requested pane?
                // * or should it just activate the pane in the sender, and leave the focused tab alone?
                //
                // For now, we'll just focus the pane in the focused tab.

                if (const auto activeTab{ _GetFocusedTabImpl() })
                {
                    _UnZoomIfNeeded();
                    args.Handled(activeTab->FocusPane(paneId));
                }
            }
        }
    }

    void TerminalPage::_HandleOpenSystemMenu(const IInspectable& /*sender*/,
                                             const ActionEventArgs& args)
    {
        OpenSystemMenu.raise(*this, nullptr);
        args.Handled(true);
    }

    void TerminalPage::_HandleExportBuffer(const IInspectable& sender,
                                           const ActionEventArgs& args)
    {
        if (const auto activeTab{ _senderOrFocusedTab(sender) })
        {
            if (args)
            {
                if (const auto& realArgs = args.ActionArgs().try_as<ExportBufferArgs>())
                {
                    _ExportTab(*activeTab, realArgs.Path());
                    args.Handled(true);
                    return;
                }
            }

            // If we didn't have args, or the args weren't ExportBufferArgs (somehow)
            _ExportTab(*activeTab, L"");
            if (args)
            {
                args.Handled(true);
            }
        }
    }

    void TerminalPage::_HandleClearBuffer(const IInspectable& /*sender*/,
                                          const ActionEventArgs& args)
    {
        if (args)
        {
            if (const auto& realArgs = args.ActionArgs().try_as<ClearBufferArgs>())
            {
                const auto res = _ApplyToActiveControls([&](auto& control) {
                    control.ClearBuffer(realArgs.Clear());
                });
                args.Handled(res);
            }
        }
    }

    void TerminalPage::_HandleMultipleActions(const IInspectable& /*sender*/,
                                              const ActionEventArgs& args)
    {
        if (args)
        {
            if (const auto& realArgs = args.ActionArgs().try_as<MultipleActionsArgs>())
            {
                for (const auto& action : realArgs.Actions())
                {
                    _actionDispatch->DoAction(action);
                }

                args.Handled(true);
            }
        }
    }

    void TerminalPage::_HandleAdjustOpacity(const IInspectable& /*sender*/,
                                            const ActionEventArgs& args)
    {
        if (args)
        {
            if (const auto& realArgs = args.ActionArgs().try_as<AdjustOpacityArgs>())
            {
                const auto res = _ApplyToActiveControls([&](auto& control) {
                    control.AdjustOpacity(realArgs.Opacity() / 100.0f, realArgs.Relative());
                });
                args.Handled(res);
            }
        }
    }

    void TerminalPage::_HandleSelectAll(const IInspectable& sender,
                                        const ActionEventArgs& args)
    {
        if (const auto& control{ _senderOrActiveControl(sender) })
        {
            control.SelectAll();
            args.Handled(true);
        }
    }

    void TerminalPage::_HandleSaveSnippet(const IInspectable& /*sender*/,
                                          const ActionEventArgs& args)
    {
        if constexpr (!Feature_SaveSnippet::IsEnabled())
        {
            return;
        }

        if (args)
        {
            if (const auto& realArgs = args.ActionArgs().try_as<SaveSnippetArgs>())
            {
                auto commandLine = realArgs.Commandline();
                if (commandLine.empty())
                {
                    if (const auto termControl{ _GetActiveControl() })
                    {
                        if (termControl.HasSelection())
                        {
                            const auto selections{ termControl.SelectedText(true) };
                            const auto selection = std::accumulate(selections.begin(), selections.end(), std::wstring());
                            commandLine = selection;
                        }
                    }
                }

                if (commandLine.empty())
                {
                    ActionSaveFailed(L"CommandLine is Required");
                    return;
                }

                try
                {
                    KeyChord keyChord = nullptr;
                    if (!realArgs.KeyChord().empty())
                    {
                        keyChord = KeyChordSerialization::FromString(winrt::to_hstring(realArgs.KeyChord()));
                    }
                    _settings.GlobalSettings().ActionMap().AddSendInputAction(realArgs.Name(), commandLine, keyChord);
                    _settings.WriteSettingsToDisk();
                    ActionSaved(commandLine, realArgs.Name(), realArgs.KeyChord());
                }
                catch (const winrt::hresult_error& ex)
                {
                    auto code = ex.code();
                    auto message = ex.message();
                    ActionSaveFailed(message);
                    args.Handled(true);
                    return;
                }

                args.Handled(true);
            }
        }
    }

    void TerminalPage::ActionSaved(winrt::hstring input, winrt::hstring name, winrt::hstring keyChord)
    {
        // If we haven't ever loaded the TeachingTip, then do so now and
        // create the toast for it.
        if (_actionSavedToast == nullptr)
        {
            if (auto tip{ FindName(L"ActionSavedToast").try_as<MUX::Controls::TeachingTip>() })
            {
                _actionSavedToast = std::make_shared<Toast>(tip);
                // Make sure to use the weak ref when setting up this
                // callback.
                tip.Closed({ get_weak(), &TerminalPage::_FocusActiveControl });
            }
        }
        _UpdateTeachingTipTheme(ActionSavedToast().try_as<winrt::Windows::UI::Xaml::FrameworkElement>());

        SavedActionName(name);
        SavedActionKeyChord(keyChord);
        SavedActionCommandLine(input);

        if (_actionSavedToast != nullptr)
        {
            _actionSavedToast->Open();
        }
    }

    void TerminalPage::ActionSaveFailed(winrt::hstring message)
    {
        // If we haven't ever loaded the TeachingTip, then do so now and
        // create the toast for it.
        if (_actionSaveFailedToast == nullptr)
        {
            if (auto tip{ FindName(L"ActionSaveFailedToast").try_as<MUX::Controls::TeachingTip>() })
            {
                _actionSaveFailedToast = std::make_shared<Toast>(tip);
                // Make sure to use the weak ref when setting up this
                // callback.
                tip.Closed({ get_weak(), &TerminalPage::_FocusActiveControl });
            }
        }
        _UpdateTeachingTipTheme(ActionSaveFailedToast().try_as<winrt::Windows::UI::Xaml::FrameworkElement>());

        ActionSaveFailedMessage().Text(message);

        if (_actionSaveFailedToast != nullptr)
        {
            _actionSaveFailedToast->Open();
        }
    }

    void TerminalPage::_HandleSelectCommand(const IInspectable& /*sender*/,
                                            const ActionEventArgs& args)
    {
        if (args)
        {
            if (const auto& realArgs = args.ActionArgs().try_as<SelectCommandArgs>())
            {
                const auto res = _ApplyToActiveControls([&](auto& control) {
                    control.SelectCommand(realArgs.Direction() == Settings::Model::SelectOutputDirection::Previous);
                });
                args.Handled(res);
            }
        }
    }
    void TerminalPage::_HandleSelectOutput(const IInspectable& /*sender*/,
                                           const ActionEventArgs& args)
    {
        if (args)
        {
            if (const auto& realArgs = args.ActionArgs().try_as<SelectOutputArgs>())
            {
                const auto res = _ApplyToActiveControls([&](auto& control) {
                    control.SelectOutput(realArgs.Direction() == Settings::Model::SelectOutputDirection::Previous);
                });
                args.Handled(res);
            }
        }
    }

    void TerminalPage::_HandleMarkMode(const IInspectable& sender,
                                       const ActionEventArgs& args)
    {
        if (const auto& control{ _senderOrActiveControl(sender) })
        {
            control.ToggleMarkMode();
            args.Handled(true);
        }
    }

    void TerminalPage::_HandleToggleBlockSelection(const IInspectable& sender,
                                                   const ActionEventArgs& args)
    {
        if (const auto& control{ _senderOrActiveControl(sender) })
        {
            const auto handled = control.ToggleBlockSelection();
            args.Handled(handled);
        }
    }

    void TerminalPage::_HandleSwitchSelectionEndpoint(const IInspectable& sender,
                                                      const ActionEventArgs& args)
    {
        if (const auto& control{ _senderOrActiveControl(sender) })
        {
            const auto handled = control.SwitchSelectionEndpoint();
            args.Handled(handled);
        }
    }

    void TerminalPage::_HandleSuggestions(const IInspectable& /*sender*/,
                                          const ActionEventArgs& args)
    {
        if (args)
        {
            if (const auto& realArgs = args.ActionArgs().try_as<SuggestionsArgs>())
            {
                _doHandleSuggestions(realArgs);
                args.Handled(true);
            }
        }
    }

    safe_void_coroutine TerminalPage::_doHandleSuggestions(SuggestionsArgs realArgs)
    {
        const auto weak = get_weak();
        const auto dispatcher = Dispatcher();
        const auto source = realArgs.Source();
        std::vector<Command> commandsCollection;
        Control::CommandHistoryContext context{ nullptr };
        winrt::hstring currentCommandline;
        winrt::hstring currentWorkingDirectory;

        // If the user wanted to use the current commandline to filter results,
        //    OR they wanted command history (or some other source that
        //       requires context from the control)
        // then get that here.
        const bool shouldGetContext = realArgs.UseCommandline() ||
                                      WI_IsAnyFlagSet(source, SuggestionsSource::CommandHistory | SuggestionsSource::QuickFixes);
        if (const auto& control{ _GetActiveControl() })
        {
            currentWorkingDirectory = control.WorkingDirectory();

            if (shouldGetContext)
            {
                context = control.CommandHistory();
                if (context)
                {
                    currentCommandline = context.CurrentCommandline();
                }
            }
        }

        // Aggregate all the commands from the different sources that
        // the user selected.

        if (WI_IsFlagSet(source, SuggestionsSource::QuickFixes) &&
            context != nullptr &&
            context.QuickFixes() != nullptr)
        {
            // \ue74c --> OEM icon
            const auto recentCommands = Command::HistoryToCommands(context.QuickFixes(), hstring{}, false, hstring{ L"\ue74c" });
            for (const auto& t : recentCommands)
            {
                commandsCollection.push_back(t);
            }
        }

        // Tasks are all the sendInput commands the user has saved in
        // their settings file. Ask the ActionMap for those.
        if (WI_IsFlagSet(source, SuggestionsSource::Tasks))
        {
            const auto tasks = co_await _settings.GlobalSettings().ActionMap().FilterToSnippets(currentCommandline, currentWorkingDirectory);
            // ----- we may be on a background thread here -----
            for (const auto& t : tasks)
            {
                commandsCollection.push_back(t);
            }
        }

        // Command History comes from the commands in the buffer,
        // assuming the user has enabled shell integration. Get those
        // from the active control.
        if (WI_IsFlagSet(source, SuggestionsSource::CommandHistory) &&
            context != nullptr)
        {
            const auto recentCommands = Command::HistoryToCommands(context.History(), currentCommandline, false, hstring{ L"\ue81c" });
            for (const auto& t : recentCommands)
            {
                commandsCollection.push_back(t);
            }
        }

        co_await wil::resume_foreground(dispatcher);
        const auto strong = weak.get();
        if (!strong)
        {
            co_return;
        }

        // Open the palette with all these commands in it.
        _OpenSuggestions(_GetActiveControl(),
                         winrt::single_threaded_vector<Command>(std::move(commandsCollection)),
                         SuggestionsMode::Palette,
                         currentCommandline);
    }

    void TerminalPage::_HandleColorSelection(const IInspectable& /*sender*/,
                                             const ActionEventArgs& args)
    {
        if (args)
        {
            if (const auto& realArgs = args.ActionArgs().try_as<ColorSelectionArgs>())
            {
                const auto res = _ApplyToActiveControls([&](auto& control) {
                    control.ColorSelection(realArgs.Foreground(), realArgs.Background(), realArgs.MatchMode());
                });
                args.Handled(res);
            }
        }
    }

    void TerminalPage::_HandleExpandSelectionToWord(const IInspectable& /*sender*/,
                                                    const ActionEventArgs& args)
    {
        if (const auto& control{ _GetActiveControl() })
        {
            const auto handled = control.ExpandSelectionToWord();
            args.Handled(handled);
        }
    }

    void TerminalPage::_HandleToggleBroadcastInput(const IInspectable& sender,
                                                   const ActionEventArgs& args)
    {
        if (const auto activeTab{ _senderOrFocusedTab(sender) })
        {
            activeTab->ToggleBroadcastInput();
            args.Handled(true);
        }
    }

    void TerminalPage::_HandleRestartConnection(const IInspectable& sender,
                                                const ActionEventArgs& args)
    {
        if (const auto activeTab{ _senderOrFocusedTab(sender) })
        {
            if (const auto activePane{ activeTab->GetActivePane() })
            {
                _restartPaneConnection(activePane->GetContent().try_as<TerminalApp::TerminalPaneContent>(), nullptr);
            }
        }
        args.Handled(true);
    }

    void TerminalPage::_HandleShowContextMenu(const IInspectable& /*sender*/,
                                              const ActionEventArgs& args)
    {
        if (const auto& control{ _GetActiveControl() })
        {
            control.ShowContextMenu();
        }
        args.Handled(true);
    }

    void TerminalPage::_HandleOpenScratchpad(const IInspectable& sender,
                                             const ActionEventArgs& args)
    {
        if (Feature_ScratchpadPane::IsEnabled())
        {
            const auto& scratchPane{ winrt::make_self<ScratchpadContent>() };

            // This is maybe a little wacky - add our key event handler to the pane
            // we made. So that we can get actions for keys that the content didn't
            // handle.
            scratchPane->GetRoot().KeyDown({ this, &TerminalPage::_KeyDownHandler });

            const auto resultPane = std::make_shared<Pane>(*scratchPane);
            _SplitPane(_senderOrFocusedTab(sender), SplitDirection::Automatic, 0.5f, resultPane);
            args.Handled(true);
        }
    }

    void TerminalPage::_HandleOpenAbout(const IInspectable& /*sender*/,
                                        const ActionEventArgs& args)
    {
        _ShowAboutDialog();
        args.Handled(true);
    }

    void TerminalPage::_HandleQuickFix(const IInspectable& /*sender*/,
                                       const ActionEventArgs& args)
    {
        if (const auto& control{ _GetActiveControl() })
        {
            const auto handled = control.OpenQuickFixMenu();
            args.Handled(handled);
        }
    }

    void TerminalPage::_HandleOpenAgentPane(const IInspectable& /*sender*/,
                                            const ActionEventArgs& args)
    {
        OutputDebugStringW(L"[AgentPane] _HandleOpenAgentPane called\n");
        const auto activeTabPre = _GetFocusedTabImpl();
        const auto agentPanePre = activeTabPre ? activeTabPre->FindAgentPane() : nullptr;
        const bool stashedPre = agentPanePre && agentPanePre->IsHidden();
        _agentPaneLog(std::string{ "_HandleOpenAgentPane fired hasPane=" } + (agentPanePre ? "yes" : "no") + " stashed=" + (stashedPre ? "yes" : "no"));

        // Per-tab. Three cases (in priority order):
        //   * Pane stashed (hidden) → fall through to _OpenOrReuseAgentPane
        //     which unstashes via wta. Don't switch view here — restore in
        //     whatever view it had when hidden.
        //   * Pane visible, sessions view → switch to chat view.
        //   * Pane visible, chat view OR no pane → fall through.
        const auto activeTab = _GetFocusedTabImpl();
        if (activeTab)
        {
            const auto agentPane = activeTab->FindAgentPane();
            const bool isStashed = agentPane && agentPane->IsHidden();
            if (!isStashed)
            {
                if (const auto agentContent = activeTab->FindAgentPaneContent())
                {
                    if (agentContent.IsSessionsView())
                    {
                        _RequestAgentStateForTab(activeTab, "chat", std::nullopt);
                        args.Handled(true);
                        return;
                    }
                }
            }
        }

        _OpenOrReuseAgentPane(false, L"Action");
        args.Handled(true);
    }

    void TerminalPage::_HandleFocusAgentPane(const IInspectable& /*sender*/,
                                             const ActionEventArgs& args)
    {
        OutputDebugStringW(L"[AgentPane] _HandleFocusAgentPane called\n");
        _FocusAgentPane();
        args.Handled(true);
    }

    void TerminalPage::_HandleOpenBackgroundAgent(const IInspectable& /*sender*/,
                                                  const ActionEventArgs& args)
    {
        OutputDebugStringW(L"[AgentPane] _HandleOpenBackgroundAgent called\n");
        _OpenBackgroundAgentTab();
        args.Handled(true);
    }

    void TerminalPage::_HandleOpenAgentSessions(const IInspectable& /*sender*/,
                                                const ActionEventArgs& args)
    {
        OutputDebugStringW(L"[AgentPane] _HandleOpenAgentSessions called\n");
        const auto activeTabPre = _GetFocusedTabImpl();
        const auto agentPanePre = activeTabPre ? activeTabPre->FindAgentPane() : nullptr;
        const bool stashedPre = agentPanePre && agentPanePre->IsHidden();
        _agentPaneLog(std::string{ "_HandleOpenAgentSessions fired hasPane=" } + (agentPanePre ? "yes" : "no") + " stashed=" + (stashedPre ? "yes" : "no"));

        // Per-tab sessions toggle. Cases (priority order):
        //   * Pane stashed → fall through (_OpenOrReuseAgentPane unstashes
        //     in sessions view via wta echo).
        //   * Pane visible, sessions view → hide (stash).
        //   * Pane visible, chat view → switch to sessions.
        //   * No pane → spawn in sessions view.
        const auto activeTab = _GetFocusedTabImpl();
        if (activeTab)
        {
            const auto agentPane = activeTab->FindAgentPane();
            const bool isStashed = agentPane && agentPane->IsHidden();
            if (!isStashed)
            {
                if (const auto agentContent = activeTab->FindAgentPaneContent())
                {
                    if (agentContent.IsSessionsView())
                    {
                        _RequestAgentStateForTab(activeTab, std::nullopt, /*pane_open*/ false);
                        args.Handled(true);
                        return;
                    }
                }
            }
        }

        _OpenOrReuseAgentPane(/*intoSessionsView*/ true, L"SessionsAction");
        args.Handled(true);
    }

    void TerminalPage::_HandleTriggerAutofix(const IInspectable& /*sender*/,
                                              const ActionEventArgs& args)
    {
        // Per-tab: read autofix state from the active tab's AgentPaneContent.
        const auto activeTab = _GetFocusedTabImpl();
        if (!activeTab)
        {
            return;
        }
        const auto agentContent = activeTab->FindAgentPaneContent();
        if (!agentContent)
        {
            return;
        }
        const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(agentContent);
        if (!impl)
        {
            return;
        }
        using AS = winrt::TerminalApp::implementation::AgentPaneContent::AutofixState;
        const auto state = impl->GetAutofixState();
        // Open or focus the active tab's agent pane (shared by Detected and
        // Review). Opening it makes the helper observe pane_open=true and
        // flip the bar to Idle on its own.
        const auto openAgentPaneForReview = [&]() {
            const auto agentPane = activeTab->FindAgentPane();
            if (agentPane && !agentPane->IsHidden())
            {
                if (agentContent.IsSessionsView())
                {
                    _RequestAgentStateForTab(activeTab, "chat", std::nullopt);
                }
            }
            else
            {
                _OpenOrReuseAgentPane(/*intoSessionsView*/ false, L"Autofix");
            }
        };
        if (state == AS::Detected)
        {
            // "Ask the agent for a fix": open the pane, then fire the LLM.
            openAgentPaneForReview();
            Json::Value evt;
            evt["type"] = "event";
            evt["method"] = "autofix_execute_from_detected";
            Json::Value params;
            params["pane_id"] = winrt::to_string(impl->GetLastErrorPaneId());
            params["tab_id"] = winrt::to_string(activeTab->StableId());
            evt["params"] = params;
            Json::StreamWriterBuilder wb;
            wb["indentation"] = "";
            ProtocolVtSequenceReceived.raise(
                *this,
                winrt::to_hstring(Json::writeString(wb, evt)));
            args.Handled(true);
        }
        else if (state == AS::Review)
        {
            // Result is ready in the pane chat — just open it for review.
            openAgentPaneForReview();
            args.Handled(true);
        }
    }

    // Bundle WTA / Intelligent Terminal diagnostic logs into a timestamped zip on the
    // Desktop, then pop Explorer with the new file selected so the user can drag it
    // straight into a bug report. Runs entirely on a background thread — the UI is
    // never blocked even if the logs dir is large.
    static safe_void_coroutine _CreateBugReportZipAsync()
    {
        co_await winrt::resume_background();

        wil::unique_cotaskmem_string desktopRaw;
        if (FAILED(SHGetKnownFolderPath(FOLDERID_Desktop, 0, nullptr, &desktopRaw)) || !desktopRaw)
        {
            co_return;
        }
        const std::filesystem::path desktop{ desktopRaw.get() };
        // Archive the WTA log *root* (`...\logs`), not the per-version subdir,
        // so the bug report captures every version's logs plus the flat
        // hook-trace.log — the whole `logs\` tree is tarred recursively below.
        const std::filesystem::path logsDir = ::IntelligentTerminal::LogDir();
        if (logsDir.empty())
        {
            co_return;
        }

        // create_directories is a no-op if the path already exists. We do this so
        // tar always has *something* to archive, even on a brand-new install where
        // no logs have been written yet.
        std::error_code ec;
        std::filesystem::create_directories(logsDir, ec);

        SYSTEMTIME st{};
        GetLocalTime(&st);
        const auto zipName = fmt::format(L"intelligent-terminal-logs-{:04d}{:02d}{:02d}-{:02d}{:02d}{:02d}.zip",
                                          st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond);
        const auto zipPath = desktop / zipName;

        // Resolve absolute paths to tar.exe and explorer.exe up-front so we
        // never rely on PATH / current-directory lookup (binary-planting hardening).
        // tar.exe ships in System32 on Windows 10 1803+ (libarchive); explorer.exe
        // lives in the Windows directory.
        wchar_t systemDir[MAX_PATH]{};
        wchar_t windowsDir[MAX_PATH]{};
        if (!GetSystemDirectoryW(systemDir, ARRAYSIZE(systemDir)) ||
            !GetWindowsDirectoryW(windowsDir, ARRAYSIZE(windowsDir)))
        {
            co_return;
        }
        const std::filesystem::path tarExe = std::filesystem::path{ systemDir } / L"tar.exe";
        const std::filesystem::path explorerExe = std::filesystem::path{ windowsDir } / L"explorer.exe";

        // `-a` picks the archive format from the .zip extension; `-C <parent>`
        // keeps a clean top-level `logs/` folder inside the archive instead of
        // leaking an absolute path. argv[0] must still be present in lpCommandLine
        // even though lpApplicationName provides the executable.
        auto cmdline = fmt::format(LR"("{}" -a -c -f "{}" -C "{}" logs)",
                                    tarExe.wstring(), zipPath.wstring(), logsDir.parent_path().wstring());

        STARTUPINFOW si{};
        si.cb = sizeof(si);
        si.dwFlags = STARTF_USESHOWWINDOW;
        si.wShowWindow = SW_HIDE;
        PROCESS_INFORMATION pi{};
        if (!CreateProcessW(tarExe.c_str(), cmdline.data(), nullptr, nullptr, FALSE,
                            CREATE_NO_WINDOW, nullptr, nullptr, &si, &pi))
        {
            co_return;
        }

        // Be strict about the wait result: on timeout or failure, kill the child
        // so a runaway tar.exe can't outlive this action. We're already on a
        // background thread, so 60s is a soft cap — anything longer almost
        // certainly means tar is stuck on a permission/handle issue.
        const DWORD waitResult = WaitForSingleObject(pi.hProcess, 60000);
        DWORD exitCode = 1;
        if (waitResult == WAIT_OBJECT_0)
        {
            GetExitCodeProcess(pi.hProcess, &exitCode);
        }
        else
        {
            TerminateProcess(pi.hProcess, 1);
            WaitForSingleObject(pi.hProcess, 5000); // reap, best-effort
        }
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);

        if (exitCode != 0 || !std::filesystem::exists(zipPath, ec))
        {
            co_return;
        }

        // Reveal the zip in Explorer (file pre-selected) so the user can drag it
        // into a GitHub issue or email immediately.
        auto selectArgs = fmt::format(LR"(/select,"{}")", zipPath.wstring());
        SHELLEXECUTEINFOW seInfo{ 0 };
        seInfo.cbSize = sizeof(seInfo);
        seInfo.fMask = SEE_MASK_NOASYNC;
        seInfo.lpVerb = L"open";
        seInfo.lpFile = explorerExe.c_str();
        seInfo.lpParameters = selectArgs.c_str();
        seInfo.nShow = SW_SHOWNORMAL;
        LOG_IF_WIN32_BOOL_FALSE(ShellExecuteExW(&seInfo));
    }

    void TerminalPage::_HandleBugReport(const IInspectable& /*sender*/,
                                        const ActionEventArgs& args)
    {
        _CreateBugReportZipAsync();
        args.Handled(true);
    }

    void TerminalPage::_HandleShowProtocolInfo(const IInspectable& /*sender*/,
                                               const ActionEventArgs& args)
    {
        // Compute pipe name from current PID (matches what WindowEmperor creates)
        const auto pid = GetCurrentProcessId();
        const auto pipeName = fmt::format(FMT_COMPILE(L"\\\\.\\pipe\\WindowsTerminal-{}"), pid);

        // Reuse the WindowIdToast TeachingTip to display protocol info
        if (_windowIdToast == nullptr)
        {
            if (auto tip{ FindName(L"WindowIdToast").try_as<MUX::Controls::TeachingTip>() })
            {
                _windowIdToast = std::make_shared<Toast>(tip);
                tip.IsLightDismissEnabled(false);
                tip.Closed({ get_weak(), &TerminalPage::_FocusActiveControl });
            }
        }
        _UpdateTeachingTipTheme(WindowIdToast().try_as<winrt::Windows::UI::Xaml::FrameworkElement>());

        if (_windowIdToast != nullptr)
        {
            WindowIdToast().Title(RS_(L"TerminalProtocolTeachingTipTitle"));
            WindowIdToast().Subtitle(pipeName);
            _windowIdToast->Open();
        }
        args.Handled(true);
    }

    safe_void_coroutine TerminalPage::_InitShellIntegration(const ShellIntegrationTarget target)
    {
        // Publish "user clicked Install -> desired = true" SYNCHRONOUSLY,
        // before the first suspension. If we deferred this until after the
        // resume_background() below, a settings reload that publishes
        // `false` between the click and our resumption could complete its
        // uninstall first, and then this coroutine would stamp `true`
        // back over it and reinstall — leaving $PROFILE installed while
        // the toggle is off. Publishing pre-suspension makes the
        // last-writer-wins semantics depend on UI-thread ordering (which
        // matches user intent ordering), not on coroutine resume ordering.
        _shellIntegrationDesiredEnabled.store(true, std::memory_order_release);

        const auto weak = get_weak();
        const auto dispatcher = Dispatcher();

        co_await winrt::resume_background();

        // Acquire a strong reference *before* touching any more member
        // state so a page destroyed while this coroutine was queued
        // doesn't leave us chasing freed members. Mirrors the pattern in
        // _ReconcileShellIntegration.
        auto self = weak.get();
        if (!self)
        {
            co_return;
        }

        namespace SI = ::Microsoft::Terminal::ShellIntegration;

        bool desiredAtRun = true;
        bool pwshAlready = false;
        bool wpAlready = false;
        bool pwshOk = false;
        bool wpOk = false;
        {
            std::lock_guard<std::mutex> guard{ _shellIntegrationReconcileMutex };
            // Re-check after acquiring the lock. If a settings reload (toggle
            // off) raced ahead and published `false`, that is a newer
            // expression of intent than the Install button click; skip the
            // install so we don't reinstall on top of the just-completed
            // uninstall and leave the profile out of sync with the toggle.
            desiredAtRun = _shellIntegrationDesiredEnabled.load(std::memory_order_acquire);
            if (desiredAtRun)
            {
                const auto pwshResult = SI::InstallForTarget(SI::Target::Pwsh);
                const auto wpResult = SI::InstallForTarget(SI::Target::WindowsPowerShell);
                pwshAlready = pwshResult.alreadyInstalled;
                wpAlready = wpResult.alreadyInstalled;
                pwshOk = pwshResult.success;
                wpOk = wpResult.success;
            }
        }

        const bool allAlreadyInstalled = pwshAlready && wpAlready;
        const bool anyFailure = !pwshOk || !wpOk;

        co_await wil::resume_foreground(dispatcher);
        if (auto strong = weak.get())
        {
            if (!desiredAtRun)
            {
                // Auto-detection was disabled between the click and the lock.
                // No dialog: a reconcile has already brought $PROFILE in line.
            }
            else if (allAlreadyInstalled)
            {
                // Already configured — no dialog needed
            }
            else if (anyFailure)
            {
                strong->_ShowShellIntegrationDialog(
                    RS_(L"InitShellIntegrationErrorTitle"),
                    RS_(L"InitShellIntegrationErrorMessage"));
            }
            else
            {
                strong->_ShowShellIntegrationDialog(
                    RS_(L"InitShellIntegrationSuccessTitle"),
                    RS_(L"InitShellIntegrationSuccessMessage"));
            }
        }
    }

    void TerminalPage::_OnSettingsInitShellIntegration(const IInspectable& /*sender*/, const ShellIntegrationTarget target)
    {
        _InitShellIntegration(target);
    }

    // Silent install/uninstall driven by EffectiveAutoErrorDetectionEnabled.
    // Called from SetSettings on first-load and on every change of the
    // effective detection setting. No dialog — this is the background
    // reconcile that keeps $PROFILE in sync with the user's stored
    // preference (including roaming/sync arrivals on fresh machines and
    // toggle-OFF cleanup that the FRE/Settings-Save dialog path doesn't
    // perform). Install/Uninstall are both idempotent.
    safe_void_coroutine TerminalPage::_ReconcileShellIntegration()
    {
        auto weak = get_weak();
        co_await winrt::resume_background();
        auto self = weak.get();
        if (!self)
        {
            co_return;
        }

        // Serialize against any other in-flight reconcile so back-to-back
        // toggle changes (or file-watcher reload storms) can't interleave
        // an earlier Install's write after a later Uninstall and leave
        // the $PROFILE block stuck in the wrong state. Reading the
        // desired flag inside the lock means the last acquirer always
        // observes the latest UI-thread-published value, so the final
        // on-disk state matches the latest setting.
        std::lock_guard<std::mutex> guard{ _shellIntegrationReconcileMutex };
        const bool enabled = _shellIntegrationDesiredEnabled.load(std::memory_order_acquire);

        namespace SI = ::Microsoft::Terminal::ShellIntegration;
        if (enabled)
        {
            SI::InstallForTarget(SI::Target::Pwsh);
            SI::InstallForTarget(SI::Target::WindowsPowerShell);
        }
        else
        {
            SI::UninstallForTarget(SI::Target::Pwsh);
            SI::UninstallForTarget(SI::Target::WindowsPowerShell);
        }
    }

    void TerminalPage::_ShowShellIntegrationDialog(const winrt::hstring& title, const winrt::hstring& message)
    {
        if (auto presenter{ _dialogPresenter.get() })
        {
            Controls::ContentDialog dialog;
            dialog.Title(winrt::box_value(title));
            dialog.Content(winrt::box_value(message));
            dialog.CloseButtonText(RS_(L"Ok"));
            dialog.DefaultButton(Controls::ContentDialogButton::Close);
            presenter.ShowDialog(dialog);
        }
    }
}
