// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include "../TerminalSettingsModel/ApplicationState.h"
#include <fstream>

using namespace Microsoft::Console;
using namespace WEX::Logging;
using namespace WEX::TestExecution;
using namespace WEX::Common;
using namespace winrt::Microsoft::Terminal::Settings::Model;

namespace SettingsModelUnitTests
{
    // Covers the workspace-persistence APIs added to ApplicationState:
    //   SaveWorkspace / RemoveWorkspace / RenameWorkspace / TakeWorkspace /
    //   AllPersistedWorkspaces.
    // All tests operate on a throw-away ApplicationState instance pointed at
    // a temp directory, so they don't touch the real user state.
    class ApplicationStateTests
    {
        TEST_CLASS(ApplicationStateTests);

        TEST_METHOD(SaveAndLookupWorkspace);
        TEST_METHOD(RemoveWorkspaceReturnsFalseWhenMissing);
        TEST_METHOD(RenameWorkspaceMigratesEntry);
        TEST_METHOD(RenameWorkspaceNoOpForEmptyOrEqualNames);
        TEST_METHOD(RenameWorkspaceNoOpForMissingEntry);
        TEST_METHOD(TakeWorkspaceRemovesAndReturns);
        TEST_METHOD(TakeWorkspaceReturnsNullWhenMissing);
        TEST_METHOD(RemoveWorkspaceDeletesDurableBuffers);
        TEST_METHOD(OverwriteWorkspaceDeletesReplacedDurableBuffers);
        TEST_METHOD(TakeWorkspacePreservesDurableBuffers);
        TEST_METHOD(SharedWorkspaceBufferSurvivesUntilLastReference);
        TEST_METHOD(SaveLookupAndTakeShellSession);
        TEST_METHOD(SaveShellSessionOverwritesSameName);

    private:
        static std::filesystem::path _tempRoot()
        {
            auto root = std::filesystem::temp_directory_path() / L"WT_ApplicationStateTests";
            std::error_code ec;
            std::filesystem::create_directories(root, ec);
            // Best-effort clean of any leftover state.json from a prior run so
            // tests see an empty starting point.
            std::filesystem::remove(root / L"state.json", ec);
            std::filesystem::remove(root / L"elevated-state.json", ec);
            for (const auto& entry : std::filesystem::directory_iterator(root, ec))
            {
                if (entry.path().filename().wstring().starts_with(L"workspace_"))
                {
                    std::filesystem::remove(entry.path(), ec);
                }
            }
            return root;
        }

        static winrt::com_ptr<implementation::ApplicationState> _make()
        {
            return winrt::make_self<implementation::ApplicationState>(_tempRoot());
        }

        static WindowLayout _makeLayout()
        {
            WindowLayout layout;
            layout.TabLayout(winrt::single_threaded_vector<ActionAndArgs>());
            return layout;
        }

        static WindowLayout _makeLayout(const winrt::guid& sessionId)
        {
            NewTerminalArgs terminalArgs;
            terminalArgs.SessionId(sessionId);

            ActionAndArgs action;
            action.Action(ShortcutAction::NewTab);
            action.Args(NewTabArgs{ terminalArgs });

            WindowLayout layout;
            layout.TabLayout(winrt::single_threaded_vector<ActionAndArgs>({ action }));
            return layout;
        }

        static std::filesystem::path _bufferPath(const winrt::guid& sessionId, const bool elevated = false)
        {
            return _tempRoot() / (elevated ?
                                      fmt::format(FMT_COMPILE(L"workspace_elevated_{}.txt"), sessionId) :
                                      fmt::format(FMT_COMPILE(L"workspace_buffer_{}.txt"), sessionId));
        }

        static void _touch(const std::filesystem::path& path)
        {
            std::ofstream file{ path };
            file << "buffer";
        }
    };

    void ApplicationStateTests::SaveAndLookupWorkspace()
    {
        auto state = _make();
        const auto layout = _makeLayout();
        state->SaveWorkspace(L"win1", layout);

        const auto all = state->AllPersistedWorkspaces();
        VERIFY_IS_NOT_NULL(all);
        VERIFY_IS_TRUE(all.HasKey(L"win1"));
    }

    void ApplicationStateTests::RemoveWorkspaceReturnsFalseWhenMissing()
    {
        auto state = _make();
        VERIFY_IS_FALSE(state->RemoveWorkspace(L"does-not-exist"));

        state->SaveWorkspace(L"win1", _makeLayout());
        VERIFY_IS_TRUE(state->RemoveWorkspace(L"win1"));
        VERIFY_IS_FALSE(state->RemoveWorkspace(L"win1"));
    }

    void ApplicationStateTests::RenameWorkspaceMigratesEntry()
    {
        auto state = _make();
        state->SaveWorkspace(L"oldName", _makeLayout());

        VERIFY_IS_TRUE(state->RenameWorkspace(L"oldName", L"newName"));

        const auto all = state->AllPersistedWorkspaces();
        VERIFY_IS_NOT_NULL(all);
        VERIFY_IS_FALSE(all.HasKey(L"oldName"));
        VERIFY_IS_TRUE(all.HasKey(L"newName"));
    }

    void ApplicationStateTests::RenameWorkspaceNoOpForEmptyOrEqualNames()
    {
        auto state = _make();
        state->SaveWorkspace(L"win1", _makeLayout());

        VERIFY_IS_FALSE(state->RenameWorkspace(L"win1", L"win1"));
        VERIFY_IS_FALSE(state->RenameWorkspace(L"", L"win2"));

        // Renaming to an empty name removes the stale entry under the old name.
        VERIFY_IS_TRUE(state->RenameWorkspace(L"win1", L""));
        const auto all = state->AllPersistedWorkspaces();
        if (all)
        {
            VERIFY_IS_FALSE(all.HasKey(L"win1"));
            VERIFY_IS_FALSE(all.HasKey(L""));
        }

        // Calling again is now a no-op because the entry is gone.
        VERIFY_IS_FALSE(state->RenameWorkspace(L"win1", L""));
    }

    void ApplicationStateTests::RenameWorkspaceNoOpForMissingEntry()
    {
        auto state = _make();
        VERIFY_IS_FALSE(state->RenameWorkspace(L"missing", L"newName"));
    }

    void ApplicationStateTests::TakeWorkspaceRemovesAndReturns()
    {
        auto state = _make();
        state->SaveWorkspace(L"win1", _makeLayout());

        const auto taken = state->TakeWorkspace(L"win1");
        VERIFY_IS_NOT_NULL(taken);

        // Subsequent Take for the same name must return null — this is the
        // atomicity guarantee the startup path relies on.
        VERIFY_IS_NULL(state->TakeWorkspace(L"win1"));
    }

    void ApplicationStateTests::TakeWorkspaceReturnsNullWhenMissing()
    {
        auto state = _make();
        VERIFY_IS_NULL(state->TakeWorkspace(L"missing"));
    }

    void ApplicationStateTests::RemoveWorkspaceDeletesDurableBuffers()
    {
        auto state = _make();
        const winrt::guid sessionId{ 0x11111111, 0x2222, 0x3333, { 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb } };
        const auto normalPath = _bufferPath(sessionId);
        const auto elevatedPath = _bufferPath(sessionId, true);
        _touch(normalPath);
        _touch(elevatedPath);
        state->SaveWorkspace(L"win1", _makeLayout(sessionId));

        VERIFY_IS_TRUE(state->RemoveWorkspace(L"win1"));
        VERIFY_IS_FALSE(std::filesystem::exists(normalPath));
        VERIFY_IS_FALSE(std::filesystem::exists(elevatedPath));
    }

    void ApplicationStateTests::OverwriteWorkspaceDeletesReplacedDurableBuffers()
    {
        auto state = _make();
        const winrt::guid oldSessionId{ 0xaaaaaaaa, 0xbbbb, 0xcccc, { 0xdd, 0xee, 0xff, 0x00, 0x11, 0x22, 0x33, 0x44 } };
        const winrt::guid newSessionId{ 0x12345678, 0x1234, 0x5678, { 0x90, 0xab, 0xcd, 0xef, 0x12, 0x34, 0x56, 0x78 } };
        const auto oldPath = _bufferPath(oldSessionId);
        const auto newPath = _bufferPath(newSessionId);
        _touch(oldPath);
        _touch(newPath);

        state->SaveWorkspace(L"win1", _makeLayout(oldSessionId));
        state->SaveWorkspace(L"win1", _makeLayout(newSessionId));

        VERIFY_IS_FALSE(std::filesystem::exists(oldPath));
        VERIFY_IS_TRUE(std::filesystem::exists(newPath));
    }

    void ApplicationStateTests::TakeWorkspacePreservesDurableBuffers()
    {
        auto state = _make();
        const winrt::guid sessionId{ 0x87654321, 0x4321, 0x8765, { 0x09, 0xba, 0xdc, 0xfe, 0x21, 0x43, 0x65, 0x87 } };
        const auto path = _bufferPath(sessionId);
        _touch(path);
        state->SaveWorkspace(L"win1", _makeLayout(sessionId));

        VERIFY_IS_NOT_NULL(state->TakeWorkspace(L"win1"));
        VERIFY_IS_TRUE(std::filesystem::exists(path));
    }

    void ApplicationStateTests::SharedWorkspaceBufferSurvivesUntilLastReference()
    {
        auto state = _make();
        const winrt::guid sessionId{ 0x13572468, 0x2468, 0x1357, { 0x24, 0x68, 0x13, 0x57, 0x9b, 0xdf, 0xac, 0xe0 } };
        const auto path = _bufferPath(sessionId);
        _touch(path);
        const auto layout = _makeLayout(sessionId);
        state->SaveWorkspace(L"win1", layout);
        state->SaveWorkspace(L"win2", layout);

        VERIFY_IS_TRUE(state->RemoveWorkspace(L"win1"));
        VERIFY_IS_TRUE(std::filesystem::exists(path));
        VERIFY_IS_TRUE(state->RemoveWorkspace(L"win2"));
        VERIFY_IS_FALSE(std::filesystem::exists(path));
    }

    void ApplicationStateTests::SaveLookupAndTakeShellSession()
    {
        auto state = _make();
        const auto layout = _makeLayout();

        state->SaveShellSession(L"build", layout);

        VERIFY_IS_TRUE(state->AllPersistedShellSessions().Lookup(L"build") == layout);
        VERIFY_ARE_EQUAL(static_cast<uint32_t>(1), state->ShellSessionNames().Size());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"build" }, state->ShellSessionNames().GetAt(0));
        VERIFY_IS_TRUE(state->TakeShellSession(L"build") == layout);
        VERIFY_IS_NULL(state->TakeShellSession(L"build"));
    }

    void ApplicationStateTests::SaveShellSessionOverwritesSameName()
    {
        auto state = _make();
        const auto original = _makeLayout();
        const auto replacement = _makeLayout();
        replacement.LaunchMode(LaunchMode::Maximized);

        state->SaveShellSession(L"build", original);
        state->SaveShellSession(L"build", replacement);

        VERIFY_ARE_EQUAL(static_cast<uint32_t>(1), state->AllPersistedShellSessions().Size());
        VERIFY_ARE_EQUAL(LaunchMode::Maximized, state->TakeShellSession(L"build").LaunchMode().Value());
    }
}
