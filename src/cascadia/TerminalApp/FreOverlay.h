// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "FreAgentEntry.g.h"
#include "FreOverlay.g.h"

#include "../inc/FreWingetClassifier.h"
#include "../inc/FreProblemKind.h"

#include <mutex>

namespace winrt::TerminalApp::implementation
{
    struct FreAgentEntry : FreAgentEntryT<FreAgentEntry>
    {
        FreAgentEntry() = default;

        winrt::hstring Id() const { return _id; }
        void Id(const winrt::hstring& value) { _id = value; }
        winrt::hstring DisplayLabel() const { return _displayLabel; }
        void DisplayLabel(const winrt::hstring& value) { _displayLabel = value; }

    private:
        winrt::hstring _id;
        winrt::hstring _displayLabel;
    };

    struct FreOverlay : FreOverlayT<FreOverlay>
    {
        FreOverlay();

        // Initialize with settings to populate controls.
        void Initialize(const winrt::Microsoft::Terminal::Settings::Model::CascadiaSettings& settings);

        // Event — sender must be the WinRT projected type.
        til::typed_event<winrt::TerminalApp::FreOverlay, winrt::Windows::Foundation::IInspectable> Completed;

        // XAML event handlers — must be public for generated code access.
        void _OnNextButtonClick(const winrt::Windows::Foundation::IInspectable& sender,
                                const winrt::Windows::UI::Xaml::RoutedEventArgs& args);
        void _OnSaveButtonClick(const winrt::Windows::Foundation::IInspectable& sender,
                                const winrt::Windows::UI::Xaml::RoutedEventArgs& args);
        void _OnCloseButtonClick(const winrt::Windows::Foundation::IInspectable& sender,
                                 const winrt::Windows::UI::Xaml::RoutedEventArgs& args);
        void _OnAgentSelectionChanged(const winrt::Windows::Foundation::IInspectable& sender,
                                      const winrt::Windows::UI::Xaml::Controls::SelectionChangedEventArgs& args);
        void _OnSessionManagementToggled(const winrt::Windows::Foundation::IInspectable& sender,
                                         const winrt::Windows::UI::Xaml::RoutedEventArgs& args);
        void _OnAutoDetectToggled(const winrt::Windows::Foundation::IInspectable& sender,
                                  const winrt::Windows::UI::Xaml::RoutedEventArgs& args);

        // No-op kept for IDL compatibility.
        void ResetDragOffset();

    private:
        winrt::Microsoft::Terminal::Settings::Model::CascadiaSettings _settings{ nullptr };

        // Things that can block FRE completion. Aliased from the
        // free-function header in `inc/FreProblemKind.h` so the pure
        // priority-selection logic at `inc/FreSoftFailurePriority.h`
        // can be unit-tested without dragging in TerminalApp / WinRT
        // / XAML. The underlying enum's numeric order is meaningful —
        // it's the soft-failure priority used by `SelectHighestPriority`.
        //
        // WinGet install failures are not in this enum because they carry
        // richer structured state (package + failure kind + HRESULT + installer
        // exit code); those go through _ShowWingetProblem instead, which uses
        // FreWingetPackage + FreWingetFailureKind below.
        using FreProblemKind = ::Microsoft::Terminal::FreProblem::Kind;

        // Which winget-installed prerequisite a failure refers to. Used by
        // _ShowWingetProblem to pick the right package display name and
        // manual-fix URL anchor.
        enum class FreWingetPackage
        {
            Copilot = 0, // GitHub.Copilot
            Node = 1, // OpenJS.NodeJS.LTS
        };

        // Categorization of why a winget install failed. Aliased from the
        // free-function classifier in `inc/FreWingetClassifier.h` so the
        // pure HRESULT → kind logic can be unit-tested without dragging in
        // TerminalApp / WinRT / XAML.
        using FreWingetFailureKind = ::Microsoft::Terminal::FreWinget::FailureKind;

        // Show a single problem: set the error message + manual-fix link, then
        // apply that problem's remediation (toggle off the affected feature, if
        // any) and re-enable the Save button. Does not raise Completed.
        void _ShowProblem(FreProblemKind kind);

        // Show a winget install failure with package-aware, failure-kind-aware
        // text. Picks the localized template by `kind`, substitutes the
        // package display name and (for InstallerFailed / Generic) a
        // pre-formatted error code string. Re-enables Save like _ShowProblem.
        void _ShowWingetProblem(FreWingetPackage package,
                                FreWingetFailureKind kind,
                                int32_t hr,
                                uint32_t installerErrorCode);

        // Shared tail end of _ShowProblem / _ShowWingetProblem after the
        // caller has set ErrorText and computed the help URL: applies the
        // URL to the help link, makes the panel visible, refreshes the
        // agent dropdown, fires the Narrator notification, re-enables
        // editing, and parks focus on the help link.
        void _FinalizeProblemDisplay(const std::wstring& url);

        // Apply the detection→suggestion master-detail dependency: detection
        // off turns the suggestion toggle off and disables it; detection on
        // re-enables it (preserving the stored value).
        void _UpdateSuggestionEnabledState();

        // (Re)build the agent dropdown from the GPO-filtered registry, labeling
        // each entry with its live install state. Safe to call repeatedly (e.g.
        // after a save) and preserves the current selection.
        void _PopulateAgentComboBox();

        // Detect whether an executable is on PATH.
        static bool _IsAgentInstalled(const wchar_t* name);
        static bool _IsNodeInstalled();
        static bool _IsWingetInstalled();

        // ── WinGet source pre-warm coordination ─────────────────────
        // While the FRE overlay is on screen (Welcome + Settings pages),
        // pre-warm winget's source manifest cache in the background so
        // the on-Save `winget install` skips the 3-20s source refresh.
        // Single-flight per process — reentrant Initialize() calls and
        // multi-window FRE coalesce onto one running prewarm. The Save
        // handler awaits s_prewarmAction before its own winget call to
        // guarantee the two winget operations never run concurrently
        // (winget's intra-process locking is not a guaranteed contract).
        static std::mutex s_prewarmMutex;
        static winrt::Windows::Foundation::IAsyncAction s_prewarmAction;

        static void _MaybeStartPrewarm(bool copilotMissing, bool nodeMissing);
        static winrt::Windows::Foundation::IAsyncAction _RunPrewarmAsync();

        // Run a winget install asynchronously on a background thread.
        // Returns FreWingetFailureKind cast to int32_t — Success (-1) on
        // success, or one of the failure kinds otherwise. On failure, the
        // associated HRESULT and installer exit code (if any) are stored in
        // the _lastWinget* instance fields below for the caller to read.
        //
        // Per-instance state, not static: each FreOverlay window has its
        // own _lastWinget* slot, so two FRE windows installing concurrently
        // (multi-window scenario) can't clobber each other's diagnostics.
        // Within one instance, the caller (_SaveAndInstallAsync) awaits
        // Copilot before kicking off Node, so no intra-instance race either.
        winrt::Windows::Foundation::IAsyncOperation<int32_t> _WingetInstallAsync(winrt::hstring packageId);

        // Diagnostic state from the last _WingetInstallAsync call — read by
        // the caller right after `co_await` to pass into _ShowWingetProblem.
        // Both fields are reset to 0 by _WingetInstallAsync on each entry.
        int32_t _lastWingetHr{ 0 };
        uint32_t _lastWingetInstallerErrorCode{ 0 };

        // Pure HRESULT → FailureKind helpers live in
        // `inc/FreWingetClassifier.h` as `IsNetworkLikeHResult` /
        // `ClassifyWingetHResult` so they can be exercised by the
        // `ut_app` test runner without a TerminalApp dependency. Call
        // them directly via the `Microsoft::Terminal::FreWinget`
        // namespace from `_WingetInstallAsync`.

        // Run wta.exe hooks install on a background thread.
        // Returns true on success.
        static winrt::Windows::Foundation::IAsyncOperation<bool> _InstallHooksAsync(winrt::hstring agentId);


        // Perform the full save + install flow asynchronously.
        winrt::Windows::Foundation::IAsyncAction _SaveAndInstallAsync();

        // Flip the overlay between "saving / installing in progress" and
        // "idle / editable" states. While saving: a modal SavingOverlay
        // covers the settings form with a centered ProgressRing +
        // "Setting up..." text, the form underneath is disabled
        // (blocks keyboard too — pointer is caught by the overlay's
        // Background), and the Save button is disabled. On error or
        // completion the inverse is applied so the user can edit and
        // retry (or click Save again).
        void _SetSavingState(bool saving);
    };
}

namespace winrt::TerminalApp::factory_implementation
{
    BASIC_FACTORY(FreAgentEntry);
    BASIC_FACTORY(FreOverlay);
}
