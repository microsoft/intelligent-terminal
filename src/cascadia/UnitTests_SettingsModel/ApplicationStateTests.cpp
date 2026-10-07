// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include "../TerminalSettingsModel/ApplicationState.h"
#include "../TerminalSettingsModel/CascadiaSettings.h"
#include <til/io.h>

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
        TEST_METHOD(SidebarMigrationPersistsAndRespectsLaterHorizontal);
        TEST_METHOD(SidebarMigrationFailedSaveDoesNotComplete);
        TEST_METHOD(SidebarMigrationFailedStateSaveRestoresLayout);
        TEST_METHOD(SidebarMigrationFreshAndAlreadySidebar);
        TEST_METHOD(SidebarIntroductionDeferralAndDuplicateWindows);
        TEST_METHOD(SidebarStaleStateCannotEraseCompletion);
        TEST_METHOD(SidebarIntroductionFailedPersistenceRemainsEligible);
        TEST_METHOD(SidebarFlagsAreSharedNotElevatedLocal);
        TEST_METHOD(SidebarIntroductionPersistenceFailureAfterPresentation);
        TEST_METHOD(SidebarMigrationStateFailureWarnsAndRollsBackRealSettings);
        TEST_METHOD(SidebarMigrationRollbackFailureWarnsAndKeepsLastSavedLayout);
        TEST_METHOD(SidebarIntroductionCloseRetriesPendingDurability);
        TEST_METHOD(SidebarIntroductionFlushCompletesPendingDurability);

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

        static std::filesystem::path _sidebarRoot()
        {
            GUID guid{};
            THROW_IF_FAILED(CoCreateGuid(&guid));
            wchar_t name[39]{};
            VERIFY_IS_TRUE(StringFromGUID2(guid, name, ARRAYSIZE(name)) > 0);
            const auto root = std::filesystem::current_path() / L"SidebarStateUnitTests" / name;
            std::filesystem::create_directories(root);
            return root;
        }

        static winrt::com_ptr<implementation::CascadiaSettings> _sidebarSettings(std::string_view layout)
        {
            auto json = std::string{ R"({"profiles":[{"guid":"{6239a42c-0000-49a3-80bd-e8fdd045185c}"}])" };
            if (!layout.empty())
            {
                json += R"(,"tabLayout":")";
                json += layout;
                json += '"';
            }
            json += '}';
            return winrt::make_self<implementation::CascadiaSettings>(std::string_view{ json }, std::string_view{ "{}" });
        }

        static wil::unique_handle _denyReplacement(const std::filesystem::path& path)
        {
            wil::unique_handle handle{ CreateFileW(path.c_str(), GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr) };
            THROW_LAST_ERROR_IF(!handle);
            return handle;
        }

        static Json::Value _readFixture(const std::filesystem::path& path)
        {
            const auto data = til::io::read_file_as_utf8_string_if_exists(path);
            Json::Value root;
            std::string errors;
            const std::unique_ptr<Json::CharReader> reader{ Json::CharReaderBuilder{}.newCharReader() };
            VERIFY_IS_TRUE(reader->parse(data.data(), data.data() + data.size(), &root, &errors));
            return root;
        }

        static bool _saveFixture(const implementation::CascadiaSettings& settings, const std::filesystem::path& path)
        {
            try
            {
                til::io::write_utf8_string_to_file_atomic(path, Json::writeString(Json::StreamWriterBuilder{}, settings.ToJson()));
                return true;
            }
            catch (...)
            {
                LOG_CAUGHT_EXCEPTION();
                return false;
            }
        }

        static bool _hasSaveWarning(const implementation::CascadiaSettings& settings)
        {
            uint32_t index = 0;
            return settings.Warnings().IndexOf(SettingsLoadWarnings::FailedToWriteToSettings, index);
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

    void ApplicationStateTests::SidebarMigrationPersistsAndRespectsLaterHorizontal()
    {
        const auto root = _sidebarRoot();
        const auto cleanup = wil::scope_exit([&]() { std::filesystem::remove_all(root); });
        auto state = winrt::make_self<implementation::ApplicationState>(root);
        auto settings = _sidebarSettings("horizontal");
        auto lock = state->LockSidebarState();
        auto saves = 0;
        VERIFY_IS_TRUE(settings->MigrateSidebarLayout(*state, [&]() {
            ++saves;
            VERIFY_ARE_EQUAL(TabLayout::Vertical, settings->GlobalSettings().TabLayout());
            VERIFY_ARE_EQUAL(std::string{ "vertical" }, settings->ToJson()["tabLayout"].asString());
            return true;
        }));
        VERIFY_ARE_EQUAL(1, saves);
        VERIFY_IS_TRUE(state->SidebarLayoutMigrationCompleted());
        VERIFY_IS_FALSE(state->SidebarIntroductionShown());
        lock.reset();

        // A new process reads completion from state.json, not a startup flag.
        auto restarted = winrt::make_self<implementation::ApplicationState>(root);
        auto laterHorizontal = _sidebarSettings("horizontal");
        lock = restarted->LockSidebarState();
        VERIFY_IS_TRUE(laterHorizontal->MigrateSidebarLayout(*restarted, [&]() { ++saves; return true; }));
        VERIFY_ARE_EQUAL(1, saves);
        VERIFY_ARE_EQUAL(TabLayout::Horizontal, laterHorizontal->GlobalSettings().TabLayout());
    }

    void ApplicationStateTests::SidebarMigrationFailedSaveDoesNotComplete()
    {
        const auto root = _sidebarRoot();
        const auto cleanup = wil::scope_exit([&]() { std::filesystem::remove_all(root); });
        auto state = winrt::make_self<implementation::ApplicationState>(root);
        auto settings = _sidebarSettings("horizontal");
        const auto lock = state->LockSidebarState();
        VERIFY_IS_FALSE(settings->MigrateSidebarLayout(*state, []() { return false; }));
        VERIFY_IS_TRUE(_hasSaveWarning(*settings));
        VERIFY_IS_FALSE(state->SidebarLayoutMigrationCompleted());
        VERIFY_ARE_EQUAL(TabLayout::Horizontal, settings->GlobalSettings().TabLayout());
        VERIFY_IS_TRUE(settings->MigrateSidebarLayout(*state, []() { return true; }));
        VERIFY_IS_TRUE(state->SidebarLayoutMigrationCompleted());
    }

    void ApplicationStateTests::SidebarMigrationFailedStateSaveRestoresLayout()
    {
        const auto root = _sidebarRoot();
        const auto cleanup = wil::scope_exit([&]() { std::filesystem::remove_all(root); });
        auto state = winrt::make_self<implementation::ApplicationState>(root);
        auto settings = _sidebarSettings("horizontal");
        const auto lock = state->LockSidebarState();
        // A directory at the file path deterministically makes persistence fail.
        std::filesystem::create_directory(root / L"state.json");
        auto saves = 0;
        VERIFY_IS_FALSE(settings->MigrateSidebarLayout(*state, [&]() { ++saves; return true; }));
        VERIFY_ARE_EQUAL(2, saves);
        VERIFY_IS_FALSE(state->SidebarLayoutMigrationCompleted());
        VERIFY_ARE_EQUAL(TabLayout::Horizontal, settings->GlobalSettings().TabLayout());
    }

    void ApplicationStateTests::SidebarMigrationFreshAndAlreadySidebar()
    {
        for (const auto layout : { "", "vertical" })
        {
            const auto root = _sidebarRoot();
            const auto cleanup = wil::scope_exit([&]() { std::filesystem::remove_all(root); });
            auto state = winrt::make_self<implementation::ApplicationState>(root);
            auto settings = _sidebarSettings(layout);
            const auto lock = state->LockSidebarState();
            VERIFY_IS_TRUE(settings->MigrateSidebarLayout(*state, []() { return true; }));
            VERIFY_IS_TRUE(state->SidebarLayoutMigrationCompleted());
            VERIFY_IS_FALSE(state->SidebarIntroductionShown());
            VERIFY_ARE_EQUAL(TabLayout::Vertical, settings->GlobalSettings().TabLayout());
        }
    }

    void ApplicationStateTests::SidebarIntroductionDeferralAndDuplicateWindows()
    {
        const auto root = _sidebarRoot();
        const auto cleanup = wil::scope_exit([&]() { std::filesystem::remove_all(root); });
        auto first = winrt::make_self<implementation::ApplicationState>(root);
        auto second = winrt::make_self<implementation::ApplicationState>(root);
        const auto deferred = first->TryBeginSidebarIntroduction();
        VERIFY_IS_TRUE(deferred != 0);
        VERIFY_IS_FALSE(first->SidebarIntroductionShown());
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, first->TryBeginSidebarIntroduction());
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, second->TryBeginSidebarIntroduction());
        first->EndSidebarIntroduction(deferred + 1, false);
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, second->TryBeginSidebarIntroduction());
        first->EndSidebarIntroduction(deferred, false);
        VERIFY_IS_FALSE(first->SidebarIntroductionShown());
        const auto presented = second->TryBeginSidebarIntroduction();
        VERIFY_IS_TRUE(presented != 0);
        second->EndSidebarIntroduction(presented, true);
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, first->TryBeginSidebarIntroduction());
        auto restarted = winrt::make_self<implementation::ApplicationState>(root);
        VERIFY_IS_TRUE(restarted->SidebarIntroductionShown());
        VERIFY_IS_FALSE(restarted->SidebarLayoutMigrationCompleted());
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, restarted->TryBeginSidebarIntroduction());
    }

    void ApplicationStateTests::SidebarStaleStateCannotEraseCompletion()
    {
        const auto root = _sidebarRoot();
        const auto cleanup = wil::scope_exit([&]() { std::filesystem::remove_all(root); });
        auto stale = winrt::make_self<implementation::ApplicationState>(root);
        auto first = winrt::make_self<implementation::ApplicationState>(root);
        {
            const auto lock = first->LockSidebarState();
            VERIFY_IS_TRUE(first->CompleteSidebarLayoutMigration());
        }
        const auto claim = first->TryBeginSidebarIntroduction();
        first->EndSidebarIntroduction(claim, true);
        first->Flush();
        stale->AgentWelcomeShown(true);
        stale->Flush();
        auto restarted = winrt::make_self<implementation::ApplicationState>(root);
        VERIFY_IS_TRUE(restarted->SidebarLayoutMigrationCompleted());
        VERIFY_IS_TRUE(restarted->SidebarIntroductionShown());
    }

    void ApplicationStateTests::SidebarIntroductionFailedPersistenceRemainsEligible()
    {
        const auto root = _sidebarRoot();
        const auto cleanup = wil::scope_exit([&]() { std::filesystem::remove_all(root); });
        auto state = winrt::make_self<implementation::ApplicationState>(root);
        std::filesystem::create_directory(root / L"state.json");
        VERIFY_THROWS(state->TryBeginSidebarIntroduction(), std::exception);
        VERIFY_IS_FALSE(state->SidebarIntroductionShown());
        std::filesystem::remove(root / L"state.json");
        const auto claim = state->TryBeginSidebarIntroduction();
        VERIFY_IS_TRUE(claim != 0);
        state->EndSidebarIntroduction(claim, false);
        VERIFY_IS_FALSE(state->SidebarIntroductionShown());
    }

    void ApplicationStateTests::SidebarFlagsAreSharedNotElevatedLocal()
    {
        const auto root = _sidebarRoot();
        const auto cleanup = wil::scope_exit([&]() { std::filesystem::remove_all(root); });
        auto state = winrt::make_self<implementation::ApplicationState>(root);
        state->SidebarLayoutMigrationCompleted(true);
        state->SidebarIntroductionShown(true);
        const auto shared = state->ToJson(implementation::FileSource::Shared);
        const auto local = state->ToJson(implementation::FileSource::Local);
        VERIFY_IS_TRUE(shared["sidebarLayoutMigrationCompleted"].asBool());
        VERIFY_IS_TRUE(shared["sidebarIntroductionShown"].asBool());
        VERIFY_IS_FALSE(local.isMember("sidebarLayoutMigrationCompleted"));
        VERIFY_IS_FALSE(local.isMember("sidebarIntroductionShown"));
    }

    void ApplicationStateTests::SidebarIntroductionPersistenceFailureAfterPresentation()
    {
        const auto root = _sidebarRoot();
        const auto cleanup = wil::scope_exit([&]() { std::filesystem::remove_all(root); });
        auto state = winrt::make_self<implementation::ApplicationState>(root);
        auto second = winrt::make_self<implementation::ApplicationState>(root);
        const auto claim = state->TryBeginSidebarIntroduction();
        VERIFY_IS_TRUE(claim != 0);
        auto denyStateWrite = _denyReplacement(root / L"state.json");
        VERIFY_THROWS(state->EndSidebarIntroduction(claim, true), std::exception);
        VERIFY_IS_TRUE(state->SidebarIntroductionShown());
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, state->TryBeginSidebarIntroduction());
        // Consume the ordinary writer's queued attempt while storage remains
        // unavailable. It must neither deadlock on itself nor release exclusion.
        state->Flush();
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, second->TryBeginSidebarIntroduction());
        denyStateWrite.reset();
        VERIFY_IS_FALSE(_readFixture(root / L"state.json")["sidebarIntroductionShown"].asBool());
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, second->TryBeginSidebarIntroduction());
        VERIFY_IS_FALSE(static_cast<bool>(second->LockSidebarState(false)));
        state->EndSidebarIntroduction(claim, true);
        VERIFY_IS_TRUE(_readFixture(root / L"state.json")["sidebarIntroductionShown"].asBool());
        VERIFY_IS_TRUE(static_cast<bool>(second->LockSidebarState(false)));
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, second->TryBeginSidebarIntroduction());
        auto restarted = winrt::make_self<implementation::ApplicationState>(root);
        VERIFY_IS_TRUE(restarted->SidebarIntroductionShown());
    }

    void ApplicationStateTests::SidebarMigrationStateFailureWarnsAndRollsBackRealSettings()
    {
        const auto root = _sidebarRoot();
        const auto cleanup = wil::scope_exit([&]() { std::filesystem::remove_all(root); });
        const auto path = root / L"settings.json";
        til::io::write_utf8_string_to_file_atomic(root / L"state.json", "{}");
        auto state = winrt::make_self<implementation::ApplicationState>(root);
        auto settings = _sidebarSettings("horizontal");
        VERIFY_IS_TRUE(_saveFixture(*settings, path));
        const auto transactionLock = state->LockSidebarState();
        auto denyStateWrite = _denyReplacement(root / L"state.json");
        auto saves = 0;
        VERIFY_IS_FALSE(settings->MigrateSidebarLayout(*state, [&]() { ++saves; return _saveFixture(*settings, path); }));
        VERIFY_ARE_EQUAL(2, saves);
        VERIFY_IS_TRUE(_hasSaveWarning(*settings));
        VERIFY_ARE_EQUAL(TabLayout::Horizontal, settings->GlobalSettings().TabLayout());
        VERIFY_ARE_EQUAL(std::string{ "horizontal" }, _readFixture(path)["tabLayout"].asString());
        VERIFY_IS_FALSE(state->SidebarLayoutMigrationCompleted());
        VERIFY_IS_FALSE(_readFixture(root / L"state.json")["sidebarLayoutMigrationCompleted"].asBool());
    }

    void ApplicationStateTests::SidebarMigrationRollbackFailureWarnsAndKeepsLastSavedLayout()
    {
        const auto root = _sidebarRoot();
        const auto cleanup = wil::scope_exit([&]() { std::filesystem::remove_all(root); });
        const auto path = root / L"settings.json";
        til::io::write_utf8_string_to_file_atomic(root / L"state.json", "{}");
        auto state = winrt::make_self<implementation::ApplicationState>(root);
        auto settings = _sidebarSettings("horizontal");
        VERIFY_IS_TRUE(_saveFixture(*settings, path));
        const auto transactionLock = state->LockSidebarState();
        auto denyStateWrite = _denyReplacement(root / L"state.json");
        wil::unique_handle denyRollback;
        auto saves = 0;
        VERIFY_IS_FALSE(settings->MigrateSidebarLayout(*state, [&]() {
            ++saves;
            const auto saved = _saveFixture(*settings, path);
            if (saves == 1 && saved)
            {
                denyRollback = _denyReplacement(path);
            }
            return saved;
        }));
        VERIFY_ARE_EQUAL(2, saves);
        VERIFY_IS_TRUE(_hasSaveWarning(*settings));
        VERIFY_ARE_EQUAL(TabLayout::Vertical, settings->GlobalSettings().TabLayout());
        VERIFY_ARE_EQUAL(std::string{ "vertical" }, _readFixture(path)["tabLayout"].asString());
        VERIFY_IS_FALSE(state->SidebarLayoutMigrationCompleted());
        VERIFY_IS_FALSE(_readFixture(root / L"state.json")["sidebarLayoutMigrationCompleted"].asBool());
    }

    void ApplicationStateTests::SidebarIntroductionCloseRetriesPendingDurability()
    {
        const auto root = _sidebarRoot();
        const auto cleanup = wil::scope_exit([&]() { std::filesystem::remove_all(root); });
        auto first = winrt::make_self<implementation::ApplicationState>(root);
        auto second = winrt::make_self<implementation::ApplicationState>(root);
        const auto claim = first->TryBeginSidebarIntroduction();
        auto denyStateWrite = _denyReplacement(root / L"state.json");
        VERIFY_THROWS(first->EndSidebarIntroduction(claim, true), std::exception);
        first->Flush();
        // Closing cannot turn an already-presented tip into a cancellation.
        VERIFY_THROWS(first->EndSidebarIntroduction(claim, false), std::exception);
        first->EndSidebarIntroduction(claim + 1, false);
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, second->TryBeginSidebarIntroduction());
        denyStateWrite.reset();
        first->EndSidebarIntroduction(claim, false);
        VERIFY_IS_TRUE(_readFixture(root / L"state.json")["sidebarIntroductionShown"].asBool());
        VERIFY_IS_TRUE(static_cast<bool>(second->LockSidebarState(false)));
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, second->TryBeginSidebarIntroduction());
    }

    void ApplicationStateTests::SidebarIntroductionFlushCompletesPendingDurability()
    {
        const auto root = _sidebarRoot();
        const auto cleanup = wil::scope_exit([&]() { std::filesystem::remove_all(root); });
        auto first = winrt::make_self<implementation::ApplicationState>(root);
        auto second = winrt::make_self<implementation::ApplicationState>(root);
        const auto claim = first->TryBeginSidebarIntroduction();
        auto denyStateWrite = _denyReplacement(root / L"state.json");
        VERIFY_THROWS(first->EndSidebarIntroduction(claim, true), std::exception);
        first->Flush();
        denyStateWrite.reset();
        VERIFY_IS_FALSE(_readFixture(root / L"state.json")["sidebarIntroductionShown"].asBool());
        VERIFY_IS_FALSE(static_cast<bool>(second->LockSidebarState(false)));
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, second->TryBeginSidebarIntroduction());
        first->Flush();
        VERIFY_IS_TRUE(_readFixture(root / L"state.json")["sidebarIntroductionShown"].asBool());
        VERIFY_IS_TRUE(static_cast<bool>(second->LockSidebarState(false)));
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, second->TryBeginSidebarIntroduction());
        // A page retaining the token can acknowledge writer completion safely.
        first->EndSidebarIntroduction(claim, false);
    }
}
