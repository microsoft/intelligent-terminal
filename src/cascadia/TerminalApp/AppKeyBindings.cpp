// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "AppKeyBindings.h"
#include "AgentPaneLog.h"

#include "AppKeyBindings.g.cpp"

using namespace winrt::Microsoft::Terminal;
using namespace winrt::TerminalApp;
using namespace winrt::Microsoft::Terminal::Control;

namespace winrt::TerminalApp::implementation
{
    bool AppKeyBindings::TryKeyChord(const KeyChord& kc)
    {
        const auto traceShortcut = kc.Vkey() == 'T' && WI_IsFlagSet(kc.Modifiers(), winrt::Windows::System::VirtualKeyModifiers::Control);
        const auto cmd{ _actionMap.GetActionByKeyChord(kc) };
        if (traceShortcut)
        {
            _agentPaneLog(fmt::format(
                "ShortcutTrace pid={} tid={} stage=control-binding modifiers={} matched={} action={}",
                GetCurrentProcessId(),
                GetCurrentThreadId(),
                static_cast<uint32_t>(kc.Modifiers()),
                static_cast<bool>(cmd),
                cmd ? static_cast<int>(cmd.ActionAndArgs().Action()) : -1));
        }
        if (!cmd)
        {
            return false;
        }
        const auto action = cmd.ActionAndArgs();
        const auto handled = (action.Action() == winrt::Microsoft::Terminal::Settings::Model::ShortcutAction::ToggleSidebar ||
                              action.Action() == winrt::Microsoft::Terminal::Settings::Model::ShortcutAction::SwitchToTab) ?
                                 _dispatch.DoAction(kc, action) :
                                 _dispatch.DoAction(action);
        if (traceShortcut)
        {
            _agentPaneLog(fmt::format("ShortcutTrace pid={} tid={} stage=control-dispatch handled={}",
                                      GetCurrentProcessId(),
                                      GetCurrentThreadId(),
                                      handled));
        }
        return handled;
    }

    bool AppKeyBindings::IsKeyChordExplicitlyUnbound(const KeyChord& kc)
    {
        return _actionMap.IsKeyChordExplicitlyUnbound(kc);
    }

    void AppKeyBindings::SetDispatch(const winrt::TerminalApp::ShortcutActionDispatch& dispatch)
    {
        _dispatch = dispatch;
    }

    void AppKeyBindings::SetActionMap(const winrt::Microsoft::Terminal::Settings::Model::IActionMapView& actionMap)
    {
        _actionMap = actionMap;
    }
}
