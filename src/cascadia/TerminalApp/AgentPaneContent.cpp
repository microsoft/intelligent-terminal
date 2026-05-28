// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "AgentPaneContent.h"
#include "AgentPaneContent.g.cpp"

#include <algorithm>
#include <cwctype>
#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.UI.Xaml.Media.Imaging.h>
#include <winrt/Windows.UI.Xaml.Media.h>

using namespace winrt::Windows::UI;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Controls;
using namespace winrt::Windows::UI::Xaml::Media;
using namespace winrt::Microsoft::Terminal::Control;
using namespace winrt::Microsoft::Terminal::Settings::Model;

namespace winrt::TerminalApp::implementation
{
    namespace
    {
        // Map the agent's display name (case-insensitive substring) to the
        // packaged white-filled SVG. Unknown agents fall back to Copilot.
        std::wstring_view _logoFileForAgent(const winrt::hstring& name)
        {
            std::wstring lower{ name };
            std::transform(lower.begin(), lower.end(), lower.begin(),
                           [](wchar_t c) { return static_cast<wchar_t>(std::towlower(c)); });
            if (lower.find(L"claude") != std::wstring::npos) return L"claude.svg";
            if (lower.find(L"codex") != std::wstring::npos) return L"codex.svg";
            if (lower.find(L"openai") != std::wstring::npos) return L"codex.svg";
            if (lower.find(L"gpt") != std::wstring::npos) return L"codex.svg";
            if (lower.find(L"gemini") != std::wstring::npos) return L"gemini.svg";
            return L"copilot.svg";
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
                                             const winrt::hstring& state)
    {
        const bool nameChanged = _agentName != name;
        _agentName = name;
        _agentVersion = version;
        _agentModel = model;
        _agentState = state;
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

    // Swap the bar between two modes:
    //   * chat / connecting / etc. (active=false) — agent logo + "<name> <version>"
    //   * session-management view  (active=true)  — no logo, "Agent sessions"
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

    void AgentPaneContent::SetAgentPanePosition(const winrt::hstring& position)
    {
        if (_agentPanePosition == position)
        {
            return;
        }
        _agentPanePosition = position;
        StateChanged.raise(*this, nullptr);
    }

    void AgentPaneContent::_refreshLabel()
    {
        // Session-management view takes over the bar — the wta TUI below no
        // longer renders its own "Agent sessions" header, so this is where
        // that title lives.
        if (_isSessionsView)
        {
            const auto text = _agentName.empty() ?
                                  std::wstring{ RS_(L"AgentPane_SessionsTitle") } :
                                  RS_fmt(L"AgentPane_SessionsTitleFormat", std::wstring{ _agentName });
            AgentLabelText().Text(winrt::hstring{ text });
            return;
        }

        std::wstring text;
        if (_agentName.empty())
        {
            text = L"";
        }
        else
        {
            text = std::wstring{ _agentName };
            if (!_agentVersion.empty())
            {
                text += L" ";
                text += _agentVersion;
            }
            else if (!_agentModel.empty())
            {
                text += L" ";
                text += _agentModel;
            }
        }
        AgentLabelText().Text(winrt::hstring{ text });
    }

    void AgentPaneContent::_refreshLogo()
    {
        if (_isSessionsView)
        {
            AgentLogo().Source(nullptr);
            AgentLogo().Visibility(Visibility::Collapsed);
            return;
        }

        if (_agentName.empty())
        {
            AgentLogo().Source(nullptr);
            AgentLogo().Visibility(Visibility::Collapsed);
            return;
        }

        std::wstring uri{ L"ms-appx:///AgentIcons/" };
        uri.append(_logoFileForAgent(_agentName));
        const winrt::Windows::Foundation::Uri parsed{ winrt::hstring{ uri } };
        winrt::Windows::UI::Xaml::Media::Imaging::SvgImageSource source{ parsed };
        source.RasterizePixelWidth(28.0);
        source.RasterizePixelHeight(28.0);
        AgentLogo().Source(source);
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
        // Re-pick up the pane position in case settings changed it.
        SetAgentPanePosition(settings.GlobalSettings().AgentPanePosition());
    }

    winrt::Windows::Foundation::Size AgentPaneContent::MinimumSize()
    {
        // Reserve 36px (top bar) on top of the inner control's minimum.
        // The bottom bar is window-level chrome now, so it isn't part of
        // this pane's minimum size.
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            const auto inner = impl->MinimumSize();
            return { inner.Width, inner.Height + 36.0f };
        }
        return { 1, 36.0f };
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
        if (const auto& impl = winrt::get_self<implementation::TerminalPaneContent>(_inner))
        {
            impl->Close();
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
