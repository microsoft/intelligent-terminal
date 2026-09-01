// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "AgentPaneContent.h"
#include "AgentPaneContent.g.cpp"
#include "AgentPaneLog.h"

#include <algorithm>
#include <cwctype>
#include <winrt/Windows.UI.Xaml.Automation.h>
#include <winrt/Windows.UI.Xaml.Media.h>

using namespace winrt::Windows::UI;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Automation;
using namespace winrt::Windows::UI::Xaml::Controls;
using namespace winrt::Windows::UI::Xaml::Media;
using namespace winrt::Microsoft::Terminal::Control;
using namespace winrt::Microsoft::Terminal::Settings::Model;
using namespace winrt::Microsoft::Terminal::TerminalConnection;

namespace winrt::TerminalApp::implementation
{
    namespace
    {
        enum class AgentLogoKind
        {
            Copilot,
            Claude,
            Gemini,
            Codex,
            OpenCode,
        };

        constexpr auto AgentHelperExitTimeout{ std::chrono::seconds{ 3 } };

        safe_void_coroutine _EnsureAgentHelperExited(wil::unique_handle process, const DWORD pid)
        {
            co_await winrt::resume_background();

            const auto waitResult = WaitForSingleObject(process.get(), static_cast<DWORD>(
                                                                           std::chrono::duration_cast<std::chrono::milliseconds>(
                                                                               AgentHelperExitTimeout)
                                                                               .count()));
            const auto waitError = waitResult == WAIT_FAILED ? GetLastError() : ERROR_SUCCESS;
            if (waitResult == WAIT_OBJECT_0)
            {
                _agentPaneLog("wta-helper exited after pane close pid=" + std::to_string(pid));
                co_return;
            }

            if (waitResult == WAIT_TIMEOUT)
            {
                _agentPaneLog("wta-helper did not exit after pane close; checking before termination pid=" + std::to_string(pid));
            }
            else if (waitResult == WAIT_FAILED)
            {
                LOG_WIN32_MSG(waitError, "Waiting for wta-helper after pane close failed (pid=%lu)", pid);
                _agentPaneLog("waiting for wta-helper after pane close failed error=" + std::to_string(waitError) + "; checking before termination pid=" + std::to_string(pid));
            }
            else
            {
                _agentPaneLog("waiting for wta-helper after pane close returned unexpected result=" + std::to_string(waitResult) + "; checking before termination pid=" + std::to_string(pid));
            }

            // The helper may have exited between the initial wait and forced cleanup.
            const auto preTerminateWaitResult = WaitForSingleObject(process.get(), 0);
            const auto preTerminateWaitError = preTerminateWaitResult == WAIT_FAILED ? GetLastError() : ERROR_SUCCESS;
            if (preTerminateWaitResult == WAIT_OBJECT_0)
            {
                _agentPaneLog("wta-helper exited before forced termination pid=" + std::to_string(pid));
                co_return;
            }
            if (preTerminateWaitResult == WAIT_FAILED)
            {
                LOG_WIN32_MSG(preTerminateWaitError, "Rechecking wta-helper before forced termination failed (pid=%lu)", pid);
                _agentPaneLog("rechecking wta-helper before forced termination failed error=" + std::to_string(preTerminateWaitError) + " pid=" + std::to_string(pid));

                DWORD exitCode = STILL_ACTIVE;
                if (GetExitCodeProcess(process.get(), &exitCode))
                {
                    if (exitCode != STILL_ACTIVE)
                    {
                        _agentPaneLog("wta-helper had already exited before forced termination pid=" + std::to_string(pid));
                        co_return;
                    }
                }
                else
                {
                    const auto exitCodeError = GetLastError();
                    LOG_WIN32_MSG(exitCodeError, "Querying wta-helper exit code before forced termination failed (pid=%lu)", pid);
                    _agentPaneLog("querying wta-helper exit code before forced termination failed error=" + std::to_string(exitCodeError) + " pid=" + std::to_string(pid));
                }
            }

            _agentPaneLog("terminating wta-helper after pane close pid=" + std::to_string(pid));
            if (!TerminateProcess(process.get(), 1))
            {
                const auto terminateError = GetLastError();
                LOG_WIN32_MSG(terminateError, "Terminating wta-helper after pane close failed (pid=%lu)", pid);
                _agentPaneLog("terminating wta-helper after pane close failed error=" + std::to_string(terminateError) + " pid=" + std::to_string(pid));
            }

            const auto reapResult = WaitForSingleObject(process.get(), 5000);
            if (reapResult == WAIT_OBJECT_0)
            {
                _agentPaneLog("wta-helper reaped after forced termination pid=" + std::to_string(pid));
            }
            else if (reapResult == WAIT_TIMEOUT)
            {
                _agentPaneLog("timed out waiting to reap wta-helper after forced termination pid=" + std::to_string(pid));
            }
            else if (reapResult == WAIT_FAILED)
            {
                const auto reapError = GetLastError();
                LOG_WIN32_MSG(reapError, "Waiting to reap wta-helper after forced termination failed (pid=%lu)", pid);
                _agentPaneLog("waiting to reap wta-helper after forced termination failed error=" + std::to_string(reapError) + " pid=" + std::to_string(pid));
            }
            else
            {
                _agentPaneLog("waiting to reap wta-helper after forced termination returned unexpected result=" + std::to_string(reapResult) + " pid=" + std::to_string(pid));
            }
        }

        wil::unique_handle _DuplicateAgentHelperProcess(const winrt::TerminalApp::TerminalPaneContent& inner)
        {
            const auto impl = winrt::get_self<implementation::TerminalPaneContent>(inner);
            if (!impl)
            {
                return {};
            }
            const auto control = impl->GetTermControl();
            const auto connection = control ? control.Connection() : nullptr;
            const auto conpty = connection.try_as<ConptyConnection>();
            const auto processValue = conpty ? conpty.RootProcessHandle() : 0;
            if (!processValue)
            {
                return {};
            }

            wil::unique_handle duplicate;
            LOG_IF_WIN32_BOOL_FALSE(DuplicateHandle(
                GetCurrentProcess(),
                reinterpret_cast<HANDLE>(processValue),
                GetCurrentProcess(),
                duplicate.addressof(),
                SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                FALSE,
                0));
            return duplicate;
        }

        // Map the agent's display name (case-insensitive substring) to its
        // XAML path. Unknown agents fall back to Copilot.
        AgentLogoKind _logoForAgent(const winrt::hstring& name)
        {
            std::wstring lower{ name };
            std::transform(lower.begin(), lower.end(), lower.begin(),
                           [](wchar_t c) { return static_cast<wchar_t>(std::towlower(c)); });
            if (lower.find(L"claude") != std::wstring::npos) return AgentLogoKind::Claude;
            if (lower.find(L"codex") != std::wstring::npos) return AgentLogoKind::Codex;
            if (lower.find(L"openai") != std::wstring::npos) return AgentLogoKind::Codex;
            if (lower.find(L"gpt") != std::wstring::npos) return AgentLogoKind::Codex;
            if (lower.find(L"gemini") != std::wstring::npos) return AgentLogoKind::Gemini;
            if (lower.find(L"opencode") != std::wstring::npos) return AgentLogoKind::OpenCode;
            return AgentLogoKind::Copilot;
        }

    }

    AgentPaneContent::AgentPaneContent(const winrt::TerminalApp::TerminalPaneContent& inner) :
        _inner{ inner }
    {
        InitializeComponent();

        // The wta TermControl is owned by the inner TerminalPaneContent.
        // Its GetRoot() returns the TermControl itself; pin it into our row 1.
        if (_inner)
        {
            InnerContent().Content(_inner.GetRoot());
        }

        _wireInnerEvents();

        // Default label + logo until wta sends an agent_status event.
        _refreshLabel();
        _refreshLogo();
    }

    winrt::TerminalApp::TerminalPaneContent AgentPaneContent::GetTerminalContent()
    {
        return _inner;
    }

    winrt::Microsoft::Terminal::Control::TermControl AgentPaneContent::GetTermControl()
    {
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            return impl->GetTermControl();
        }
        return nullptr;
    }

    void AgentPaneContent::UpdateAgentStatus(const winrt::hstring& name,
                                             const winrt::hstring& version,
                                             const winrt::hstring& model,
                                             const winrt::hstring& state,
                                             const winrt::hstring& backend)
    {
        _helperEventReady = true;
        const bool nameChanged = _agentName != name;
        _agentName = name;
        _agentVersion = version;
        _agentModel = model;
        _agentState = state;
        _agentBackend = backend;
        _refreshLabel();
        if (nameChanged)
        {
            _refreshLogo();
        }
        // Match `SetSessionsView` and `ApplyAutofixState`: any bottom-bar-
        // affecting state mutation on AgentPaneContent must raise
        // `StateChanged` so subscribers (TerminalPage's bar-refresh
        // handler) can pick up the change without polling. The bar does
        // not currently render the agent name itself, but the cross-
        // window-drag fix path in `TabManagement.cpp` relies on
        // `_UpdateBottomBarState` running once after the wire-up to
        // reflect any cached state the helper pushed before the wire
        // was in place — without the raise here, that catch-up wouldn't
        // observe agent_status arriving in the same race window. Also
        // future-proofs the bar against ever displaying agent name.
        StateChanged.raise(*this, nullptr);
    }

    void AgentPaneContent::UpdateYoloStatus(const winrt::hstring& state, const winrt::hstring& detail)
    {
        const auto status = AgentYoloStatusText();
        std::wstring text;
        if (state == L"pending_on")
        {
            text = L"◌ Yolo · ";
            text += RS_(L"FreOverlay_ToggleOn");
            text += L"…";
        }
        else if (state == L"pending_off")
        {
            text = L"◌ Yolo · ";
            text += RS_(L"FreOverlay_ToggleOff");
            text += L"…";
        }
        else if (state == L"pending_config")
        {
            text = L"◌ Yolo…";
        }
        else if (state == L"active")
        {
            text = L"● Yolo · ";
            text += RS_(L"FreOverlay_ToggleOn");
        }
        else if (state == L"off")
        {
            text = L"○ Yolo · ";
            text += RS_(L"FreOverlay_ToggleOff");
        }
        else if (state == L"unavailable")
        {
            text = L"⚠ Yolo";
        }
        else if (state == L"unknown")
        {
            text = L"? Yolo";
        }

        const auto badge = winrt::hstring{ text };
        status.Text(badge);
        status.Visibility(text.empty() ? Visibility::Collapsed : Visibility::Visible);
        auto accessibleName = text;
        if (!detail.empty())
        {
            accessibleName += L": ";
            accessibleName += detail;
        }
        AutomationProperties::SetName(status, winrt::hstring{ accessibleName });
        AutomationProperties::SetHelpText(status, detail);
        if (detail.empty())
        {
            ToolTipService::SetToolTip(status, nullptr);
        }
        else
        {
            ToolTipService::SetToolTip(status, box_value(detail));
        }
    }

    // Swap the bar between two modes. Both keep the agent logo and the
    // "<agent> · <backend>" identity so the pane reads the same either way:
    //   * chat / connecting / etc. (active=false) — identity + version + model
    //   * session management view (active=true)   — identity inside the
    //     "Agent sessions: {0}" title
    // Idempotent so callers don't need to dedupe.
    void AgentPaneContent::SetSessionsView(bool active)
    {
        if (_isSessionsView == active)
        {
            return;
        }
        _isSessionsView = active;
        _refreshLabel();
        _refreshLogo();
        StateChanged.raise(*this, nullptr);
    }

    void AgentPaneContent::ApplyAutofixState(AutofixState state,
                                             const winrt::hstring& paneId,
                                             const winrt::hstring& summary,
                                             const winrt::hstring& fixPreview,
                                             const winrt::hstring& hotkeyHint,
                                             const winrt::hstring& suggestionTitle)
    {
        _autofixState = state;
        if (state == AutofixState::Idle)
        {
            // Clear ALL cached fields on idle, including `_hotkeyHint`.
            // The bottom bar reads these directly, so a leftover hint
            // from a prior Detected/Pending transition would otherwise
            // hang around after the bar should have gone quiet.
            _lastErrorPaneId = {};
            _fixPreview = {};
            _suggestionTitle = {};
            _detectedSummary = {};
            _hotkeyHint = {};
        }
        else
        {
            if (!paneId.empty())
            {
                _lastErrorPaneId = paneId;
            }
            if (!summary.empty())
            {
                _detectedSummary = summary;
            }
            if (!fixPreview.empty())
            {
                _fixPreview = fixPreview;
            }
            if (!hotkeyHint.empty())
            {
                _hotkeyHint = hotkeyHint;
            }
            if (!suggestionTitle.empty())
            {
                _suggestionTitle = suggestionTitle;
            }
        }
        StateChanged.raise(*this, nullptr);
    }

    bool AgentPaneContent::ApplyAgentUsage(const Json::Value& usage)
    {
        return ::TerminalApp::AgentUsage::TryUpdateCache(_agentUsage, usage);
    }

    void AgentPaneContent::SetAgentPanePosition(const winrt::hstring& position)
    {
        if (_agentPanePosition == position)
        {
            return;
        }
        _agentPanePosition = position;
        StateChanged.raise(*this, nullptr);
    }

    // Apply the supplied colors to the agent-pane top bar (#348). The vector
    // logo paths bind to the label's foreground, so both take the same tint.
    // The bottom border uses the TabViewBackground theme resource.
    void AgentPaneContent::ApplyThemeColors(const Media::Brush& background,
                                            const Media::Brush& foreground)
    {
        if (const auto barRoot = AgentBarRoot())
        {
            barRoot.Background(background);
        }
        if (const auto label = AgentLabelText())
        {
            label.Foreground(foreground);
        }
    }

    void AgentPaneContent::_refreshLabel()
    {
        std::wstring text;
        if (_agentName.empty())
        {
            // No agent name yet. The chat view names the assistant and its
            // connection state; the sessions view falls through to its own
            // no-agent title below.
            if (!_isSessionsView)
            {
                text = _agentState == L"connecting" ?
                           std::wstring{ RS_(L"AgentPane_ConnectingTitle") } :
                           std::wstring{ RS_(L"AgentPane_DefaultTitle") };
            }
        }
        else
        {
            // Both views lead with the same agent identity ("Copilot · Debian")
            // so the bar doesn't appear to change agents when the user opens
            // session management. Only the chat view appends the version and
            // model — those describe the live conversation, not the session
            // list, and the sessions rows carry their own per-row detail.
            text = std::wstring{ _agentName };
            if (!_agentBackend.empty())
            {
                text += L" \u00B7 ";
                text += _agentBackend;
            }
            if (!_isSessionsView)
            {
                if (!_agentVersion.empty())
                {
                    text += L" ";
                    text += _agentVersion;
                }
                if (_agentState == L"connected" && !_agentModel.empty())
                {
                    text += L" \u00B7 ";
                    text += _agentModel;
                }
            }
        }

        // The session-management view takes over the bar — the wta TUI below
        // no longer renders its own "Agent sessions" header, so that title
        // lives here and keeps naming the view even once the agent is known.
        if (_isSessionsView)
        {
            text = text.empty() ?
                       std::wstring{ RS_(L"AgentPane_SessionsTitle") } :
                       RS_fmt(L"AgentPane_SessionsTitleFormat", text);
        }
        AgentLabelText().Text(winrt::hstring{ text });
    }

    void AgentPaneContent::_refreshLogo()
    {
        // The logo stays up in the session-management view: the bar keeps
        // showing which agent (and backend) owns the pane, so hiding the
        // mark there would make the two views look unrelated.
        if (_agentName.empty())
        {
            AgentLogo().Visibility(Visibility::Collapsed);
            return;
        }

        const auto logo = _logoForAgent(_agentName);
        CopilotLogo().Visibility(logo == AgentLogoKind::Copilot ? Visibility::Visible : Visibility::Collapsed);
        ClaudeLogo().Visibility(logo == AgentLogoKind::Claude ? Visibility::Visible : Visibility::Collapsed);
        GeminiLogo().Visibility(logo == AgentLogoKind::Gemini ? Visibility::Visible : Visibility::Collapsed);
        CodexLogo().Visibility(logo == AgentLogoKind::Codex ? Visibility::Visible : Visibility::Collapsed);
        OpenCodeLogo().Visibility(logo == AgentLogoKind::OpenCode ? Visibility::Visible : Visibility::Collapsed);
        AgentLogo().Visibility(Visibility::Visible);
    }

#pragma region IPaneContent forwarding
    winrt::Windows::UI::Xaml::FrameworkElement AgentPaneContent::GetRoot()
    {
        return *this;
    }

    void AgentPaneContent::UpdateSettings(const CascadiaSettings& settings)
    {
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            impl->UpdateSettings(settings);
        }

        const winrt::Microsoft::Terminal::Control::KeyChord ctrlV{ Windows::System::VirtualKeyModifiers::Control, 'V', 0 };
        if (const auto actionMap = settings.ActionMap())
        {
            const auto command = actionMap.GetActionByKeyChord(ctrlV);
            const auto isPasteAction = command && command.ActionAndArgs().Action() == ShortcutAction::PasteText;
            GetTermControl().EnableAgentPasteShortcutFallback(
                !actionMap.IsKeyChordExplicitlyUnbound(ctrlV) && (!command || isPasteAction));
        }
        else
        {
            GetTermControl().EnableAgentPasteShortcutFallback(false);
        }
    }

    winrt::Windows::Foundation::Size AgentPaneContent::MinimumSize()
    {
        // The responsive TUI's hard floor is seven rows: input(3),
        // activity(1), chat(1), and a compact recommendation(2). Reserve
        // six additional grid rows beyond TermControl's existing one-row
        // minimum, plus the fixed 36px agent bar.
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            const auto inner = impl->MinimumSize();
            const auto rowHeight = std::max(1.0f, impl->GridUnitSize().Height);
            return { inner.Width, inner.Height + (6.0f * rowHeight) + 36.0f };
        }
        return { 1, 43.0f };
    }

    void AgentPaneContent::Focus(winrt::Windows::UI::Xaml::FocusState reason)
    {
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            impl->Focus(reason);
        }
    }

    void AgentPaneContent::Close()
    {
        _unwireInnerEvents();
        auto helperProcess = _helperTransferredForDrag ? wil::unique_handle{} : _DuplicateAgentHelperProcess(_inner);
        const auto helperPid = helperProcess ? GetProcessId(helperProcess.get()) : 0;
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            impl->Close();
        }
        if (_helperTransferredForDrag)
        {
            _agentPaneLog("skipping wta-helper exit enforcement for cross-window transfer");
        }
        if (helperProcess)
        {
            _EnsureAgentHelperExited(std::move(helperProcess), helperPid);
        }
    }

    INewContentArgs AgentPaneContent::GetNewTerminalArgs(BuildStartupKind kind) const
    {
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            return impl->GetNewTerminalArgs(kind);
        }
        return nullptr;
    }

    winrt::hstring AgentPaneContent::Title()
    {
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            return impl->Title();
        }
        return L"Agent";
    }

    uint64_t AgentPaneContent::TaskbarState()
    {
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            return impl->TaskbarState();
        }
        return 0;
    }

    uint64_t AgentPaneContent::TaskbarProgress()
    {
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            return impl->TaskbarProgress();
        }
        return 0;
    }

    bool AgentPaneContent::ReadOnly()
    {
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            return impl->ReadOnly();
        }
        return false;
    }

    winrt::hstring AgentPaneContent::Icon() const
    {
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            return impl->Icon();
        }
        return {};
    }

    Windows::Foundation::IReference<winrt::Windows::UI::Color> AgentPaneContent::TabColor() const noexcept
    {
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            return impl->TabColor();
        }
        return nullptr;
    }

    winrt::Windows::UI::Xaml::Media::Brush AgentPaneContent::BackgroundBrush()
    {
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            return impl->BackgroundBrush();
        }
        return nullptr;
    }
#pragma endregion

#pragma region ISnappable
    float AgentPaneContent::SnapDownToGrid(const TerminalApp::PaneSnapDirection direction, const float sizeToSnap)
    {
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            // Snapping is computed against the terminal grid; account for the
            // 36px we steal off the top before delegating, then add it back.
            if (direction == TerminalApp::PaneSnapDirection::Height)
            {
                const auto adjusted = std::max(0.0f, sizeToSnap - 36.0f);
                return impl->SnapDownToGrid(direction, adjusted) + 36.0f;
            }
            return impl->SnapDownToGrid(direction, sizeToSnap);
        }
        return sizeToSnap;
    }

    Windows::Foundation::Size AgentPaneContent::GridUnitSize()
    {
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            return impl->GridUnitSize();
        }
        return { 1, 1 };
    }
#pragma endregion

#pragma region inner event forwarding
    void AgentPaneContent::_wireInnerEvents()
    {
        if (!_inner)
        {
            return;
        }

        // Forward each inner IPaneContent event up to our own subscribers so
        // Tab / TerminalPage can stay agnostic to the wrapper.
        const auto self = get_strong();

        _innerCloseRequested = _inner.CloseRequested(
            [self](const winrt::TerminalApp::IPaneContent& /*sender*/, const auto& args) {
                self->CloseRequested.raise(*self, args);
            });

        _innerConnectionStateChanged = _inner.ConnectionStateChanged(
            [self](const auto& sender, const auto& args) {
                self->ConnectionStateChanged.raise(sender, args);
            });

        _innerBellRequested = _inner.BellRequested(
            [self](const winrt::TerminalApp::IPaneContent& /*sender*/, const winrt::TerminalApp::BellEventArgs& args) {
                self->BellRequested.raise(*self, args);
            });

        _innerTitleChanged = _inner.TitleChanged(
            [self](const winrt::TerminalApp::IPaneContent& /*sender*/, const auto& args) {
                self->TitleChanged.raise(*self, args);
            });

        _innerTabColorChanged = _inner.TabColorChanged(
            [self](const winrt::TerminalApp::IPaneContent& /*sender*/, const auto& args) {
                self->TabColorChanged.raise(*self, args);
            });

        _innerTaskbarProgressChanged = _inner.TaskbarProgressChanged(
            [self](const winrt::TerminalApp::IPaneContent& /*sender*/, const auto& args) {
                self->TaskbarProgressChanged.raise(*self, args);
            });

        _innerReadOnlyChanged = _inner.ReadOnlyChanged(
            [self](const winrt::TerminalApp::IPaneContent& /*sender*/, const auto& args) {
                self->ReadOnlyChanged.raise(*self, args);
            });

        _innerFocusRequested = _inner.FocusRequested(
            [self](const winrt::TerminalApp::IPaneContent& /*sender*/, const auto& args) {
                self->FocusRequested.raise(*self, args);
            });
    }

    void AgentPaneContent::_unwireInnerEvents()
    {
        if (!_inner)
        {
            return;
        }
        _inner.CloseRequested(_innerCloseRequested);
        _inner.ConnectionStateChanged(_innerConnectionStateChanged);
        _inner.BellRequested(_innerBellRequested);
        _inner.TitleChanged(_innerTitleChanged);
        _inner.TabColorChanged(_innerTabColorChanged);
        _inner.TaskbarProgressChanged(_innerTaskbarProgressChanged);
        _inner.ReadOnlyChanged(_innerReadOnlyChanged);
        _inner.FocusRequested(_innerFocusRequested);
    }
#pragma endregion
}
