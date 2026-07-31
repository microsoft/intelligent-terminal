// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../TerminalApp/LocalGitProvider.h"

using namespace WEX::TestExecution;
using namespace Microsoft::Terminal::RepoAwareness;

namespace TerminalAppUnitTests
{
    class LocalGitProviderTests
    {
        TEST_CLASS(LocalGitProviderTests);

        TEST_METHOD(ReadsRepositoryWithoutNetworkAccess);

        static std::filesystem::path _repositoryRoot();
    };

    std::filesystem::path LocalGitProviderTests::_repositoryRoot()
    {
        auto root = std::filesystem::path{ __FILE__ };
        for (size_t i = 0; i < 4; ++i)
        {
            root = root.parent_path();
        }
        return root;
    }

    void LocalGitProviderTests::ReadsRepositoryWithoutNetworkAccess()
    {
        const auto git = GitProcessRunner::FindGitExecutable();
        if (!git)
        {
            return;
        }

        const auto expectedRoot = _repositoryRoot();
        const LocalGitProvider provider{ GitProcessRunner{ *git } };
        const auto result = provider.Refresh(expectedRoot / L"src" / L"cascadia" / L"ut_app", 2);

        VERIFY_ARE_EQUAL(LocalGitError::None, result.error);
        VERIFY_ARE_EQUAL(expectedRoot.lexically_normal().native(), result.snapshot.worktreeRoot.lexically_normal().native());
        VERIFY_IS_TRUE(result.snapshot.status.branch.has_value() || result.snapshot.status.detached);
        VERIFY_IS_FALSE(result.snapshot.status.headOid.empty());
        VERIFY_IS_LESS_THAN_OR_EQUAL(result.snapshot.status.files.size(), size_t{ 2 });
    }
}
