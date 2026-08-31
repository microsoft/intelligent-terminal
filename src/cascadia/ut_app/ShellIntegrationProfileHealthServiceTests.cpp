// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include <atomic>
#include <future>

#include "../TerminalApp/ShellIntegrationProfileHealthService.h"
#include "../inc/ShellIntegrationCommon.h"

using namespace Microsoft::Terminal::ShellIntegration::Health;
using namespace WEX::TestExecution;
using winrt::TerminalApp::implementation::ShellIntegrationProfileHealthService;

namespace TerminalAppUnitTests
{
    class ShellIntegrationProfileHealthServiceTests
    {
        TEST_CLASS(ShellIntegrationProfileHealthServiceTests);

        TEST_METHOD(CachesCompletedTarget);
        TEST_METHOD(CachesExistingProfileWithoutManagedBlock);
        TEST_METHOD(ForceDuringFlightRunsFreshAnalysis);
        TEST_METHOD(OldGenerationCannotCancelNewRun);
        TEST_METHOD(MissingProfileDoesNotInvokeAnalyzer);
        TEST_METHOD(ExternalEditInvalidatesCache);
        TEST_METHOD(ProfileCreatedAfterMissingResultIsAnalyzed);
        TEST_METHOD(ExternalEditDuringAnalysisIsNotCached);
    };

    namespace
    {
        struct TemporaryProfile
        {
            TemporaryProfile()
            {
                wchar_t directory[MAX_PATH]{};
                VERIFY_IS_TRUE(GetTempPathW(ARRAYSIZE(directory), directory) != 0);

                wchar_t path[MAX_PATH]{};
                VERIFY_IS_TRUE(GetTempFileNameW(directory, L"ith", 0, path) != 0);
                value = path;

                const wil::unique_hfile file{
                    CreateFileW(path, GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS, FILE_ATTRIBUTE_TEMPORARY, nullptr)
                };
                VERIFY_IS_TRUE(static_cast<bool>(file));
                constexpr std::string_view contents{ "# profile\n" };
                DWORD written{};
                VERIFY_IS_TRUE(WriteFile(
                    file.get(),
                    contents.data(),
                    static_cast<DWORD>(contents.size()),
                    &written,
                    nullptr));
                VERIFY_ARE_EQUAL(static_cast<DWORD>(contents.size()), written);
            }

            ~TemporaryProfile()
            {
                DeleteFileW(value.c_str());
            }

            TargetKey Target() const
            {
                TargetKey target;
                target.shell = ShellKind::GitBash;
                target.syntax = ProfileSyntax::Bash;
                target.profilePath = value;
                target.shellIdentity = value;
                return target;
            }

            void Write(const std::string_view contents) const
            {
                const wil::unique_hfile file{
                    CreateFileW(value.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS, FILE_ATTRIBUTE_TEMPORARY, nullptr)
                };
                VERIFY_IS_TRUE(static_cast<bool>(file));
                DWORD written{};
                VERIFY_IS_TRUE(WriteFile(
                    file.get(),
                    contents.data(),
                    static_cast<DWORD>(contents.size()),
                    &written,
                    nullptr));
                VERIFY_ARE_EQUAL(static_cast<DWORD>(contents.size()), written);
            }

            std::wstring value;
        };

        template<typename T>
        T Wait(std::future<T>& future)
        {
            VERIFY_ARE_EQUAL(std::future_status::ready, future.wait_for(std::chrono::seconds{ 10 }));
            return future.get();
        }
    }

    void ShellIntegrationProfileHealthServiceTests::CachesCompletedTarget()
    {
        TemporaryProfile profile;
        auto& service = ShellIntegrationProfileHealthService::Instance();
        service.SetEnabled(false);
        const auto generation = service.SetEnabled(true);
        std::atomic_size_t analyses{};

        auto analyze = [&](const TargetKey&, const std::string_view) {
            ++analyses;
            return AnalysisResult{ Status::Healthy, Reason::None };
        };

        std::promise<Result> firstPromise;
        auto first = firstPromise.get_future();
        service.Request(profile.Target(), generation, analyze, [&](const Result& result) {
            firstPromise.set_value(result);
        });
        VERIFY_ARE_EQUAL(Status::Healthy, Wait(first).analysis.status);

        std::promise<Result> secondPromise;
        auto second = secondPromise.get_future();
        service.Request(profile.Target(), generation, analyze, [&](const Result& result) {
            secondPromise.set_value(result);
        });
        VERIFY_ARE_EQUAL(Status::Healthy, Wait(second).analysis.status);
        VERIFY_ARE_EQUAL(size_t{ 1 }, analyses.load());

        {
            Microsoft::Terminal::ShellIntegration::details::ProfileMutationGuard mutation{ profile.value };
        }
        std::promise<Result> invalidatedPromise;
        auto invalidated = invalidatedPromise.get_future();
        service.Request(profile.Target(), generation, analyze, [&](const Result& result) {
            invalidatedPromise.set_value(result);
        });
        VERIFY_ARE_EQUAL(Status::Healthy, Wait(invalidated).analysis.status);
        VERIFY_ARE_EQUAL(size_t{ 2 }, analyses.load());
        service.SetEnabled(false);
    }

    void ShellIntegrationProfileHealthServiceTests::CachesExistingProfileWithoutManagedBlock()
    {
        TemporaryProfile profile;
        auto& service = ShellIntegrationProfileHealthService::Instance();
        service.SetEnabled(false);
        const auto generation = service.SetEnabled(true);
        std::atomic_size_t analyses{};
        auto analyze = [&](const TargetKey&, const std::string_view) {
            ++analyses;
            return AnalysisResult{ Status::NotInstalled, Reason::MissingBlock };
        };

        for (size_t request = 0; request < 2; ++request)
        {
            std::promise<Result> promise;
            auto result = promise.get_future();
            service.Request(profile.Target(), generation, analyze, [&](const Result& value) {
                promise.set_value(value);
            });
            VERIFY_ARE_EQUAL(Status::NotInstalled, Wait(result).analysis.status);
        }

        VERIFY_ARE_EQUAL(size_t{ 1 }, analyses.load());
        service.SetEnabled(false);
    }

    void ShellIntegrationProfileHealthServiceTests::ForceDuringFlightRunsFreshAnalysis()
    {
        TemporaryProfile profile;
        auto& service = ShellIntegrationProfileHealthService::Instance();
        service.SetEnabled(false);
        const auto generation = service.SetEnabled(true);

        std::promise<void> startedPromise;
        auto started = startedPromise.get_future();
        std::promise<void> releasePromise;
        const auto release = releasePromise.get_future().share();
        std::atomic_size_t analyses{};

        std::promise<Result> firstPromise;
        auto first = firstPromise.get_future();
        service.Request(
            profile.Target(),
            generation,
            [&](const TargetKey&, const std::string_view) {
                ++analyses;
                startedPromise.set_value();
                release.wait();
                return AnalysisResult{ Status::Healthy, Reason::None };
            },
            [&](const Result& result) {
                firstPromise.set_value(result);
            });
        VERIFY_ARE_EQUAL(std::future_status::ready, started.wait_for(std::chrono::seconds{ 10 }));

        std::promise<Result> forcedPromise;
        auto forced = forcedPromise.get_future();
        service.Request(
            profile.Target(),
            generation,
            [&](const TargetKey&, const std::string_view) {
                ++analyses;
                return AnalysisResult{ Status::BlockNotLast, Reason::None };
            },
            [&](const Result& result) {
                forcedPromise.set_value(result);
            },
            true);

        releasePromise.set_value();
        VERIFY_ARE_EQUAL(Status::Healthy, Wait(first).analysis.status);
        VERIFY_ARE_EQUAL(Status::BlockNotLast, Wait(forced).analysis.status);
        VERIFY_ARE_EQUAL(size_t{ 2 }, analyses.load());
        service.SetEnabled(false);
    }

    void ShellIntegrationProfileHealthServiceTests::MissingProfileDoesNotInvokeAnalyzer()
    {
        TemporaryProfile profile;
        VERIFY_IS_TRUE(DeleteFileW(profile.value.c_str()));

        auto& service = ShellIntegrationProfileHealthService::Instance();
        service.SetEnabled(false);
        const auto generation = service.SetEnabled(true);

        auto target = profile.Target();

        std::atomic_bool invoked{};
        std::promise<Result> promise;
        auto future = promise.get_future();
        service.Request(
            std::move(target),
            generation,
            [&](const TargetKey&, const std::string_view) {
                invoked = true;
                return AnalysisResult{ Status::Healthy, Reason::None };
            },
            [&](const Result& result) {
                promise.set_value(result);
            });

        const auto result = Wait(future);
        VERIFY_ARE_EQUAL(Status::NotInstalled, result.analysis.status);
        VERIFY_ARE_EQUAL(Reason::MissingBlock, result.analysis.reason);
        VERIFY_IS_FALSE(invoked.load());
        service.SetEnabled(false);
    }

    void ShellIntegrationProfileHealthServiceTests::OldGenerationCannotCancelNewRun()
    {
        TemporaryProfile profile;
        auto& service = ShellIntegrationProfileHealthService::Instance();
        service.SetEnabled(false);
        const auto oldGeneration = service.SetEnabled(true);

        std::promise<void> oldStartedPromise;
        auto oldStarted = oldStartedPromise.get_future();
        std::promise<void> releaseOldPromise;
        const auto releaseOld = releaseOldPromise.get_future().share();
        service.Request(
            profile.Target(),
            oldGeneration,
            [&](const TargetKey&, const std::string_view) {
                oldStartedPromise.set_value();
                releaseOld.wait();
                return AnalysisResult{ Status::Healthy, Reason::None };
            },
            [](const Result&) {});
        VERIFY_ARE_EQUAL(std::future_status::ready, oldStarted.wait_for(std::chrono::seconds{ 10 }));

        service.SetEnabled(false);
        const auto newGeneration = service.SetEnabled(true);
        std::promise<void> newStartedPromise;
        auto newStarted = newStartedPromise.get_future();
        std::promise<void> releaseNewPromise;
        const auto releaseNew = releaseNewPromise.get_future().share();
        std::promise<Result> newResultPromise;
        auto newResult = newResultPromise.get_future();
        service.Request(
            profile.Target(),
            newGeneration,
            [&](const TargetKey&, const std::string_view) {
                newStartedPromise.set_value();
                releaseNew.wait();
                return AnalysisResult{ Status::BlockNotLast, Reason::None };
            },
            [&](const Result& result) {
                newResultPromise.set_value(result);
            });
        VERIFY_ARE_EQUAL(std::future_status::ready, newStarted.wait_for(std::chrono::seconds{ 10 }));

        releaseOldPromise.set_value();
        std::this_thread::sleep_for(std::chrono::milliseconds{ 100 });
        releaseNewPromise.set_value();
        VERIFY_ARE_EQUAL(Status::BlockNotLast, Wait(newResult).analysis.status);
        service.SetEnabled(false);
    }

    void ShellIntegrationProfileHealthServiceTests::ExternalEditInvalidatesCache()
    {
        TemporaryProfile profile;
        auto& service = ShellIntegrationProfileHealthService::Instance();
        service.SetEnabled(false);
        const auto generation = service.SetEnabled(true);
        std::atomic_size_t analyses{};

        auto analyze = [&](const TargetKey&, const std::string_view contents) {
            ++analyses;
            return AnalysisResult{
                contents.find("changed") == std::string_view::npos ? Status::Healthy : Status::BlockNotLast,
                Reason::None
            };
        };

        std::promise<Result> firstPromise;
        auto first = firstPromise.get_future();
        service.Request(profile.Target(), generation, analyze, [&](const Result& result) {
            firstPromise.set_value(result);
        });
        VERIFY_ARE_EQUAL(Status::Healthy, Wait(first).analysis.status);

        profile.Write("# profile changed\n");

        std::promise<Result> secondPromise;
        auto second = secondPromise.get_future();
        service.Request(profile.Target(), generation, analyze, [&](const Result& result) {
            secondPromise.set_value(result);
        });
        VERIFY_ARE_EQUAL(Status::BlockNotLast, Wait(second).analysis.status);
        VERIFY_ARE_EQUAL(size_t{ 2 }, analyses.load());
        service.SetEnabled(false);
    }

    void ShellIntegrationProfileHealthServiceTests::ProfileCreatedAfterMissingResultIsAnalyzed()
    {
        TemporaryProfile profile;
        VERIFY_IS_TRUE(DeleteFileW(profile.value.c_str()));

        auto& service = ShellIntegrationProfileHealthService::Instance();
        service.SetEnabled(false);
        const auto generation = service.SetEnabled(true);
        std::atomic_size_t analyses{};
        auto analyze = [&](const TargetKey&, const std::string_view) {
            ++analyses;
            return AnalysisResult{ Status::Healthy, Reason::None };
        };

        std::promise<Result> missingPromise;
        auto missing = missingPromise.get_future();
        service.Request(profile.Target(), generation, analyze, [&](const Result& result) {
            missingPromise.set_value(result);
        });
        VERIFY_ARE_EQUAL(Status::NotInstalled, Wait(missing).analysis.status);
        VERIFY_ARE_EQUAL(size_t{ 0 }, analyses.load());

        profile.Write("# created profile\n");

        std::promise<Result> createdPromise;
        auto created = createdPromise.get_future();
        service.Request(profile.Target(), generation, analyze, [&](const Result& result) {
            createdPromise.set_value(result);
        });
        VERIFY_ARE_EQUAL(Status::Healthy, Wait(created).analysis.status);
        VERIFY_ARE_EQUAL(size_t{ 1 }, analyses.load());
        service.SetEnabled(false);
    }

    void ShellIntegrationProfileHealthServiceTests::ExternalEditDuringAnalysisIsNotCached()
    {
        TemporaryProfile profile;
        auto& service = ShellIntegrationProfileHealthService::Instance();
        service.SetEnabled(false);
        const auto generation = service.SetEnabled(true);

        std::promise<void> startedPromise;
        auto started = startedPromise.get_future();
        std::promise<void> releasePromise;
        const auto release = releasePromise.get_future().share();

        std::promise<Result> resultPromise;
        auto result = resultPromise.get_future();
        service.Request(
            profile.Target(),
            generation,
            [&](const TargetKey&, const std::string_view) {
                startedPromise.set_value();
                release.wait();
                return AnalysisResult{ Status::Healthy, Reason::None };
            },
            [&](const Result& value) {
                resultPromise.set_value(value);
            });

        VERIFY_ARE_EQUAL(std::future_status::ready, started.wait_for(std::chrono::seconds{ 10 }));
        profile.Write("# profile changed during analysis\n");
        releasePromise.set_value();

        const auto stale = Wait(result);
        VERIFY_ARE_EQUAL(Status::Indeterminate, stale.analysis.status);
        VERIFY_ARE_EQUAL(Reason::ChangedDuringAnalysis, stale.analysis.reason);

        std::promise<Result> retryPromise;
        auto retry = retryPromise.get_future();
        service.Request(
            profile.Target(),
            generation,
            [](const TargetKey&, const std::string_view) {
                return AnalysisResult{ Status::BlockNotLast, Reason::None };
            },
            [&](const Result& value) {
                retryPromise.set_value(value);
            });
        VERIFY_ARE_EQUAL(Status::BlockNotLast, Wait(retry).analysis.status);
        service.SetEnabled(false);
    }
}
