// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../TerminalApp/RepoAwarenessService.h"
#include "../TerminalApp/RepoSummarySerializer.h"

#include <future>

using namespace WEX::TestExecution;
using namespace Microsoft::Terminal::RepoAwareness;

namespace TerminalAppUnitTests
{
    class RepoAwarenessServiceTests
    {
        TEST_CLASS(RepoAwarenessServiceTests);

        TEST_METHOD(RequiresBothShellSignals);
        TEST_METHOD(DoesNoWorkWithoutConsumer);
        TEST_METHOD(DropsQueuedWorkWhenLastConsumerLeaves);
        TEST_METHOD(ExplicitPullUpgradesQueuedConsumerWork);
        TEST_METHOD(ExplicitPullRunsWithoutConsumer);
        TEST_METHOD(CoalescesRefreshesAndReturnsSummary);
        TEST_METHOD(RejectsCompletionAfterCwdChanges);
        TEST_METHOD(ReturnsStaleSnapshotWhileRefreshing);
        TEST_METHOD(ResolvesDifferentCwdsInOneWorktree);
        TEST_METHOD(DoesNotInferNestedWorktreeFromParentCache);
        TEST_METHOD(EvictsUnreferencedCacheAfterGracePeriod);
        TEST_METHOD(NotifiesMultipleSubscribersIndependently);
        TEST_METHOD(SerializesSummaryOnlyProtocolSchema);

        static LocalGitResult _success(std::wstring root, std::string branch, uint64_t modified = 0);
    };

    LocalGitResult RepoAwarenessServiceTests::_success(std::wstring root, std::string branch, const uint64_t modified)
    {
        LocalGitResult result;
        result.snapshot.worktreeRoot = std::move(root);
        result.snapshot.status.branch = std::move(branch);
        result.snapshot.status.headOid = "deadbeef";
        result.snapshot.status.modifiedCount = modified;
        return result;
    }

    void RepoAwarenessServiceTests::RequiresBothShellSignals()
    {
        std::atomic_uint calls{ 0 };
        RepoAwarenessService service{
            [&](const auto&, size_t, const auto*) {
                ++calls;
                return _success(L"C:\\repo", "main");
            }
        };
        service.AddConsumer();

        service.ObservePane("pane", L"C:\\repo", true, false, true);
        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 1 }));
        VERIFY_ARE_EQUAL(0u, calls.load());
        VERIFY_ARE_EQUAL(RepoAvailability::ShellIntegrationRequired, service.GetSummary("pane").availability);

        service.ObservePane("pane", L"C:\\repo", true, true, true);
        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 1 }));
        VERIFY_ARE_EQUAL(1u, calls.load());
        VERIFY_ARE_EQUAL(RepoAvailability::Ready, service.GetSummary("pane").availability);
    }

    void RepoAwarenessServiceTests::DoesNoWorkWithoutConsumer()
    {
        std::atomic_uint calls{ 0 };
        RepoAwarenessService service{
            [&](const auto&, size_t, const auto*) {
                ++calls;
                return _success(L"C:\\repo", "main");
            }
        };

        service.ObservePane("pane", L"C:\\repo", true, true, true);
        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 1 }));
        VERIFY_ARE_EQUAL(0u, calls.load());

        service.AddConsumer();
        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 1 }));
        VERIFY_ARE_EQUAL(1u, calls.load());
    }

    void RepoAwarenessServiceTests::DropsQueuedWorkWhenLastConsumerLeaves()
    {
        std::atomic_uint calls{ 0 };
        std::promise<void> started;
        const auto startedFuture = started.get_future();
        std::promise<void> release;
        const auto releaseFuture = release.get_future().share();
        RepoAwarenessService service{
            [&](const std::filesystem::path& cwd, size_t, const auto*) {
                if (++calls == 1)
                {
                    started.set_value();
                    releaseFuture.wait();
                }
                return _success(cwd.native(), "main");
            }
        };
        service.AddConsumer();
        service.ObservePane("running", L"C:\\running", true, true, false);
        VERIFY_ARE_EQUAL(std::future_status::ready, startedFuture.wait_for(std::chrono::seconds{ 1 }));
        service.ObservePane("queued", L"C:\\queued", true, true, false);

        service.RemoveConsumer();
        release.set_value();
        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 2 }));
        VERIFY_ARE_EQUAL(1u, calls.load());
    }

    void RepoAwarenessServiceTests::ExplicitPullRunsWithoutConsumer()
    {
        std::atomic_uint calls{ 0 };
        RepoAwarenessService service{
            [&](const auto&, size_t, const auto*) {
                ++calls;
                return _success(L"C:\\repo", "main");
            }
        };
        service.ObservePane("pane", L"C:\\repo", true, true, false);

        VERIFY_ARE_EQUAL(RepoAvailability::Loading, service.GetSummary("pane", true).availability);
        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 1 }));
        VERIFY_ARE_EQUAL(1u, calls.load());
        VERIFY_ARE_EQUAL(RepoAvailability::Ready, service.GetSummary("pane").availability);
    }

    void RepoAwarenessServiceTests::ExplicitPullUpgradesQueuedConsumerWork()
    {
        std::atomic_uint calls{ 0 };
        std::promise<void> started;
        const auto startedFuture = started.get_future();
        std::promise<void> release;
        const auto releaseFuture = release.get_future().share();
        RepoAwarenessService service{
            [&](const std::filesystem::path& cwd, size_t, const auto*) {
                if (++calls == 1)
                {
                    started.set_value();
                    releaseFuture.wait();
                }
                return _success(cwd.native(), "main");
            }
        };
        service.AddConsumer();
        service.ObservePane("blocker", L"C:\\blocker", true, true, false);
        VERIFY_ARE_EQUAL(std::future_status::ready, startedFuture.wait_for(std::chrono::seconds{ 1 }));
        service.ObservePane("explicit", L"C:\\explicit", true, true, false);

        VERIFY_ARE_EQUAL(RepoAvailability::Loading, service.GetSummary("explicit", true).availability);
        service.RemoveConsumer();
        release.set_value();
        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 2 }));
        VERIFY_ARE_EQUAL(2u, calls.load());
        VERIFY_ARE_EQUAL(RepoAvailability::Ready, service.GetSummary("explicit").availability);
    }

    void RepoAwarenessServiceTests::CoalescesRefreshesAndReturnsSummary()
    {
        std::atomic_uint calls{ 0 };
        std::promise<void> release;
        const auto ready = release.get_future().share();
        RepoAwarenessService service{
            [&](const auto&, size_t, const auto*) {
                ++calls;
                ready.wait();
                return _success(L"C:\\repo", "feature", 4);
            }
        };
        service.AddConsumer();

        service.ObservePane("pane", L"C:\\repo", true, true, true);
        service.ObservePane("pane", L"C:\\repo", true, true, true);
        service.ObservePane("pane", L"C:\\repo", true, true, true);
        release.set_value();

        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 2 }));
        VERIFY_ARE_EQUAL(1u, calls.load());
        const auto summary = service.GetSummary("pane");
        VERIFY_ARE_EQUAL(RepoAvailability::Ready, summary.availability);
        VERIFY_ARE_EQUAL(std::string{ "feature" }, *summary.branch);
        VERIFY_ARE_EQUAL(uint64_t{ 4 }, summary.modifiedCount);
        VERIFY_ARE_EQUAL(uint64_t{ 1 }, summary.generation);
    }

    void RepoAwarenessServiceTests::RejectsCompletionAfterCwdChanges()
    {
        std::promise<void> release;
        const auto ready = release.get_future().share();
        RepoAwarenessService service{
            [&](const std::filesystem::path& cwd, size_t, const auto*) {
                if (cwd == L"C:\\old")
                {
                    ready.wait();
                    return _success(L"C:\\old", "old");
                }
                return _success(L"C:\\new", "new");
            }
        };
        service.AddConsumer();

        service.ObservePane("pane", L"C:\\old", true, true, true);
        service.ObservePane("pane", L"C:\\new", true, true, true);
        release.set_value();

        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 2 }));
        const auto summary = service.GetSummary("pane");
        VERIFY_ARE_EQUAL(RepoAvailability::Ready, summary.availability);
        VERIFY_ARE_EQUAL(std::string{ "new" }, *summary.branch);
    }

    void RepoAwarenessServiceTests::ReturnsStaleSnapshotWhileRefreshing()
    {
        std::atomic_uint calls{ 0 };
        std::promise<void> releaseSecond;
        const auto ready = releaseSecond.get_future().share();
        std::atomic_bool released{ false };
        const auto release = [&] {
            if (!released.exchange(true))
            {
                releaseSecond.set_value();
            }
        };
        const auto releaseOnExit = wil::scope_exit([&] {
            release();
        });
        RepoAwarenessService service{
            [&](const auto&, size_t, const auto*) {
                if (++calls == 1)
                {
                    return _success(L"C:\\repo", "main", 1);
                }
                ready.wait();
                return _success(L"C:\\repo", "main", 2);
            },
            std::chrono::milliseconds{ 0 }
        };
        service.AddConsumer();
        service.ObservePane("pane", L"C:\\repo", true, true, true);
        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 1 }));

        const auto stale = service.GetSummary("pane");
        VERIFY_ARE_EQUAL(RepoAvailability::Ready, stale.availability);
        VERIFY_IS_TRUE(stale.stale);
        VERIFY_ARE_EQUAL(uint64_t{ 1 }, stale.modifiedCount);

        release();
        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 2 }));
        VERIFY_ARE_EQUAL(uint64_t{ 2 }, service.GetSummary("pane").modifiedCount);
    }

    void RepoAwarenessServiceTests::ResolvesDifferentCwdsInOneWorktree()
    {
        std::atomic_uint calls{ 0 };
        RepoAwarenessService service{
            [&](const auto&, size_t, const auto*) {
                ++calls;
                return _success(L"C:\\repo", "main");
            }
        };

        service.ObservePane("one", L"C:\\repo\\src", true, true, false);
        service.ObservePane("two", L"C:\\repo\\tests", true, true, false);
        service.AddConsumer();

        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 2 }));
        VERIFY_ARE_EQUAL(2u, calls.load());
        VERIFY_ARE_EQUAL(RepoAvailability::Ready, service.GetSummary("one").availability);
        VERIFY_ARE_EQUAL(RepoAvailability::Ready, service.GetSummary("two").availability);

    }

    void RepoAwarenessServiceTests::DoesNotInferNestedWorktreeFromParentCache()
    {
        std::atomic_uint calls{ 0 };
        RepoAwarenessService service{
            [&](const std::filesystem::path& cwd, size_t, const auto*) {
                ++calls;
                if (cwd == L"C:\\repo\\nested")
                {
                    return _success(L"C:\\repo\\nested", "nested");
                }
                return _success(L"C:\\repo", "parent");
            }
        };
        service.AddConsumer();

        service.ObservePane("parent", L"C:\\repo", true, true, false);
        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 1 }));
        service.ObservePane("nested", L"C:\\repo\\nested", true, true, false);
        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 1 }));

        VERIFY_ARE_EQUAL(2u, calls.load());
        VERIFY_ARE_EQUAL(std::string{ "parent" }, *service.GetSummary("parent").branch);
        VERIFY_ARE_EQUAL(std::string{ "nested" }, *service.GetSummary("nested").branch);
    }

    void RepoAwarenessServiceTests::EvictsUnreferencedCacheAfterGracePeriod()
    {
        std::atomic_uint calls{ 0 };
        RepoAwarenessService service{
            [&](const auto&, size_t, const auto*) {
                ++calls;
                return _success(L"C:\\repo", "main");
            },
            std::chrono::minutes{ 1 },
            std::chrono::milliseconds{ 0 }
        };
        service.AddConsumer();

        service.ObservePane("one", L"C:\\repo", true, true, false);
        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 1 }));
        service.RemovePane("one");
        service.ObservePane("two", L"C:\\repo", true, true, false);
        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 1 }));

        VERIFY_ARE_EQUAL(2u, calls.load());
    }

    void RepoAwarenessServiceTests::NotifiesMultipleSubscribersIndependently()
    {
        RepoAwarenessService service{
            [&](const auto&, size_t, const auto*) {
                return _success(L"C:\\repo", "main");
            }
        };
        std::atomic_uint first{ 0 };
        std::atomic_uint second{ 0 };
        const auto firstToken = service.SubscribeSummaryChanged([&](const auto&, const auto&) {
            ++first;
        });
        [[maybe_unused]] const auto secondToken = service.SubscribeSummaryChanged([&](const auto&, const auto&) {
            ++second;
        });

        service.SetConsumerCount(1);
        service.ObservePane("pane", L"C:\\repo", true, true, false);
        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 1 }));
        VERIFY_ARE_EQUAL(1u, first.load());
        VERIFY_ARE_EQUAL(1u, second.load());

        service.UnsubscribeSummaryChanged(firstToken);
        static_cast<void>(service.GetSummary("pane", true));
        VERIFY_IS_TRUE(service.WaitForIdle(std::chrono::seconds{ 1 }));
        VERIFY_ARE_EQUAL(1u, first.load());
        VERIFY_ARE_EQUAL(2u, second.load());
    }

    void RepoAwarenessServiceTests::SerializesSummaryOnlyProtocolSchema()
    {
        RepoSummary summary;
        summary.availability = RepoAvailability::Ready;
        summary.branch = "feature";
        summary.headOid = "deadbeef";
        summary.upstream = "origin/feature";
        summary.modifiedCount = 2;
        summary.filesTruncated = true;

        const auto json = SerializeRepoStateChanged("pane", summary);
        Json::CharReaderBuilder reader;
        Json::Value event;
        std::string errors;
        std::istringstream stream{ json };
        VERIFY_IS_TRUE(Json::parseFromStream(reader, stream, &event, &errors));
        VERIFY_ARE_EQUAL(std::string{ "repo_state_changed" }, event["method"].asString());
        const auto& repo = event["params"]["repo"];
        VERIFY_ARE_EQUAL(std::string{ "feature" }, repo["branch"].asString());
        VERIFY_ARE_EQUAL(Json::UInt64{ 2 }, repo["modified"].asUInt64());
        VERIFY_IS_TRUE(repo["files_truncated"].asBool());
        VERIFY_IS_FALSE(repo.isMember("files"));
        VERIFY_IS_FALSE(repo.isMember("paths"));
        VERIFY_IS_FALSE(repo.isMember("worktree_root"));
    }
}
