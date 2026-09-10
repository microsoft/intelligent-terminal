// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "AgentPaneContent.h"
#include "AgentPaneContent.g.cpp"
#include "AgentPaneLog.h"
#include "../inc/AgentPaneRestore.h"

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

        // Map the agent's display name (case-insensitive substring) to its
        // XAML path. Unknown agents fall back to Copilot.
        AgentLogoKind _logoForAgent(const winrt::hstring& name)
        {
            std::wstring lower{ name };
            std::transform(lower.begin(), lower.end(), lower.begin(), [](wchar_t c) { return static_cast<wchar_t>(std::towlower(c)); });
            if (lower.find(L"claude") != std::wstring::npos)
                return AgentLogoKind::Claude;
            if (lower.find(L"codex") != std::wstring::npos)
                return AgentLogoKind::Codex;
            if (lower.find(L"openai") != std::wstring::npos)
                return AgentLogoKind::Codex;
            if (lower.find(L"gpt") != std::wstring::npos)
                return AgentLogoKind::Codex;
            if (lower.find(L"gemini") != std::wstring::npos)
                return AgentLogoKind::Gemini;
            if (lower.find(L"opencode") != std::wstring::npos)
                return AgentLogoKind::OpenCode;
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
        // Status changes invalidate the bottom bar, including updates received
        // while a transferred pane's event handlers are being installed.
        StateChanged.raise(*this, nullptr);
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
        GetTermControl().EnableAgentMouseWheelZoom(true);

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
        if (std::exchange(_closed, true))
        {
            return;
        }
        _unwireInnerEvents();
        _lifetime.CaptureHelperProcess();
        auto closeOnFailure = wil::scope_exit([&]() noexcept { _lifetime.Close(); });
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            impl->Close();
            _lifetime.RetireClosedContent();
            closeOnFailure.release();
        }
    }

    INewContentArgs AgentPaneContent::GetNewTerminalArgs(BuildStartupKind kind) const
    {
        const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner);
        if (!impl)
        {
            return nullptr;
        }

        auto args = impl->GetNewTerminalArgs(kind);

        // Keep a discriminator on live moves so a late/failed receive cannot
        // relaunch the helper command as an ordinary terminal.
        if (kind == BuildStartupKind::Content || kind == BuildStartupKind::MovePane)
        {
            if (const auto terminalArgs = args.try_as<NewTerminalArgs>())
            {
                terminalArgs.SetContentType(winrt::hstring{ ::Microsoft::Terminal::AgentPaneRestore::PaneType });
                terminalArgs.AgentPaneTransferId(_transferId);
            }
            return args;
        }

        if (kind != BuildStartupKind::Persist)
        {
            return args;
        }

        const auto terminalArgs = args.try_as<winrt::Microsoft::Terminal::Settings::Model::NewTerminalArgs>();
        if (!terminalArgs)
        {
            return args;
        }

        // Mark the pane as agent-backed. `Pane::GetTerminalArgsForPane`
        // upgrades this to the stashed variant when the pane is hidden.
        terminalArgs.SetContentType(winrt::hstring{ ::Microsoft::Terminal::AgentPaneRestore::PaneType });

        // Replace the live helper command line — which names this run's master
        // pipe, owner ids and resolved CLI path — with the stable resume form.
        ::Microsoft::Terminal::AgentPaneRestore::Fields fields;
        fields.sessionId = _agentSessionId;
        fields.view = _isSessionsView ? ::Microsoft::Terminal::AgentPaneRestore::SessionsView : ::Microsoft::Terminal::AgentPaneRestore::ChatView;
        fields.agentIdentity = _agentRestoreIdentity;
        fields.customCommand = _agentRestoreCustomCommand;
        fields.yoloControlOwner = _yoloControlOwner;
        terminalArgs.Commandline(winrt::hstring{ ::Microsoft::Terminal::AgentPaneRestore::BuildPaneCommandline(_wtaExecutablePath, fields) });

        return args;
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
        const auto weak = get_weak();

        _innerCloseRequested = _inner.CloseRequested(
            [weak](const winrt::TerminalApp::IPaneContent& /*sender*/, const auto& args) {
                if (const auto self = weak.get())
                {
                    self->CloseRequested.raise(*self, args);
                }
            });

        _innerConnectionStateChanged = _inner.ConnectionStateChanged(
            [weak](const auto& sender, const auto& args) {
                if (const auto self = weak.get())
                {
                    self->ConnectionStateChanged.raise(sender, args);
                }
            });

        _innerBellRequested = _inner.BellRequested(
            [weak](const winrt::TerminalApp::IPaneContent& /*sender*/, const winrt::TerminalApp::BellEventArgs& args) {
                if (const auto self = weak.get())
                {
                    self->BellRequested.raise(*self, args);
                }
            });

        _innerTitleChanged = _inner.TitleChanged(
            [weak](const winrt::TerminalApp::IPaneContent& /*sender*/, const auto& args) {
                if (const auto self = weak.get())
                {
                    self->TitleChanged.raise(*self, args);
                }
            });

        _innerTabColorChanged = _inner.TabColorChanged(
            [weak](const winrt::TerminalApp::IPaneContent& /*sender*/, const auto& args) {
                if (const auto self = weak.get())
                {
                    self->TabColorChanged.raise(*self, args);
                }
            });

        _innerTaskbarProgressChanged = _inner.TaskbarProgressChanged(
            [weak](const winrt::TerminalApp::IPaneContent& /*sender*/, const auto& args) {
                if (const auto self = weak.get())
                {
                    self->TaskbarProgressChanged.raise(*self, args);
                }
            });

        _innerReadOnlyChanged = _inner.ReadOnlyChanged(
            [weak](const winrt::TerminalApp::IPaneContent& /*sender*/, const auto& args) {
                if (const auto self = weak.get())
                {
                    self->ReadOnlyChanged.raise(*self, args);
                }
            });

        _innerFocusRequested = _inner.FocusRequested(
            [weak](const winrt::TerminalApp::IPaneContent& /*sender*/, const auto& args) {
                if (const auto self = weak.get())
                {
                    self->FocusRequested.raise(*self, args);
                }
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
