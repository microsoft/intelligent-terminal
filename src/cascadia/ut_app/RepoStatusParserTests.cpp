// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../TerminalApp/RepoStatusParser.h"

using namespace WEX::TestExecution;
using namespace Microsoft::Terminal::RepoAwareness;

namespace TerminalAppUnitTests
{
    class RepoStatusParserTests
    {
        TEST_CLASS(RepoStatusParserTests);

        TEST_METHOD(ParsesBranchAndAllChangeKinds);
        TEST_METHOD(ParsesDetachedAndUnbornHeads);
        TEST_METHOD(RetainsRawRenamePaths);
        TEST_METHOD(CountsCompleteStreamWhenFilesAreTruncated);
        TEST_METHOD(RejectsMalformedOrIncompleteRecords);
    };

    void RepoStatusParserTests::ParsesBranchAndAllChangeKinds()
    {
        static constexpr char input[]{
            "# branch.oid 0123456789abcdef\0"
            "# branch.head feature/repo-aware\0"
            "# branch.upstream origin/feature/repo-aware\0"
            "# branch.ab +3 -2\0"
            "1 .M N... 100644 100644 100644 aaaaaaa bbbbbbb src/modified.cpp\0"
            "1 A. N... 000000 100644 100644 0000000 ccccccc src/staged.cpp\0"
            "1 MM S.M. 100644 100644 100644 ddddddd eeeeeee deps/module\0"
            "? src/new file.cpp\0"
            "u UU N... 100644 100644 100644 100644 aaaaaaa bbbbbbb ccccccc conflict.txt\0"
        };

        const auto result = ParseRepoStatus(std::string_view{ input, sizeof(input) - 1 }, 100);

        VERIFY_IS_TRUE(static_cast<bool>(result));
        VERIFY_ARE_EQUAL(std::string{ "feature/repo-aware" }, *result.snapshot.branch);
        VERIFY_ARE_EQUAL(std::string{ "0123456789abcdef" }, result.snapshot.headOid);
        VERIFY_ARE_EQUAL(std::string{ "origin/feature/repo-aware" }, *result.snapshot.upstream);
        VERIFY_ARE_EQUAL(uint64_t{ 3 }, result.snapshot.ahead);
        VERIFY_ARE_EQUAL(uint64_t{ 2 }, result.snapshot.behind);
        VERIFY_ARE_EQUAL(uint64_t{ 2 }, result.snapshot.modifiedCount);
        VERIFY_ARE_EQUAL(uint64_t{ 2 }, result.snapshot.stagedCount);
        VERIFY_ARE_EQUAL(uint64_t{ 1 }, result.snapshot.untrackedCount);
        VERIFY_ARE_EQUAL(uint64_t{ 1 }, result.snapshot.conflictedCount);
        VERIFY_ARE_EQUAL(size_t{ 5 }, result.snapshot.files.size());
        VERIFY_IS_TRUE(result.snapshot.files[2].submodule);
    }

    void RepoStatusParserTests::ParsesDetachedAndUnbornHeads()
    {
        static constexpr char detached[]{ "# branch.oid deadbeef\0# branch.head (detached)\0" };
        const auto detachedResult = ParseRepoStatus(std::string_view{ detached, sizeof(detached) - 1 }, 10);
        VERIFY_IS_TRUE(static_cast<bool>(detachedResult));
        VERIFY_IS_TRUE(detachedResult.snapshot.detached);
        VERIFY_IS_FALSE(detachedResult.snapshot.branch.has_value());

        static constexpr char unborn[]{ "# branch.oid (initial)\0# branch.head main\0" };
        const auto unbornResult = ParseRepoStatus(std::string_view{ unborn, sizeof(unborn) - 1 }, 10);
        VERIFY_IS_TRUE(static_cast<bool>(unbornResult));
        VERIFY_IS_TRUE(unbornResult.snapshot.unborn);
        VERIFY_ARE_EQUAL(std::string{ "main" }, *unbornResult.snapshot.branch);
        VERIFY_IS_TRUE(unbornResult.snapshot.headOid.empty());
    }

    void RepoStatusParserTests::RetainsRawRenamePaths()
    {
        std::string input{
            "2 R. N... 100644 100644 100644 aaaaaaa bbbbbbb R100 dst/"
        };
        input.push_back(static_cast<char>(0xff));
        input.append(".txt", 4);
        input.push_back('\0');
        input.append("src/old name.txt", 16);
        input.push_back('\0');

        const auto result = ParseRepoStatus(input, 10);

        VERIFY_IS_TRUE(static_cast<bool>(result));
        VERIFY_ARE_EQUAL(size_t{ 1 }, result.snapshot.files.size());
        VERIFY_ARE_EQUAL(size_t{ 9 }, result.snapshot.files[0].path.size());
        VERIFY_ARE_EQUAL(static_cast<char>(0xff), result.snapshot.files[0].path[4]);
        VERIFY_ARE_EQUAL(std::string{ "src/old name.txt" }, *result.snapshot.files[0].originalPath);
        VERIFY_ARE_EQUAL(uint64_t{ 1 }, result.snapshot.stagedCount);
    }

    void RepoStatusParserTests::CountsCompleteStreamWhenFilesAreTruncated()
    {
        static constexpr char input[]{
            "1 .M N... 100644 100644 100644 aaaaaaa bbbbbbb one\0"
            "1 A. N... 000000 100644 100644 0000000 ccccccc two\0"
            "? three\0"
            "? four\0"
        };

        const auto result = ParseRepoStatus(std::string_view{ input, sizeof(input) - 1 }, 1);

        VERIFY_IS_TRUE(static_cast<bool>(result));
        VERIFY_ARE_EQUAL(size_t{ 1 }, result.snapshot.files.size());
        VERIFY_IS_TRUE(result.snapshot.filesTruncated);
        VERIFY_ARE_EQUAL(uint64_t{ 1 }, result.snapshot.modifiedCount);
        VERIFY_ARE_EQUAL(uint64_t{ 1 }, result.snapshot.stagedCount);
        VERIFY_ARE_EQUAL(uint64_t{ 2 }, result.snapshot.untrackedCount);
    }

    void RepoStatusParserTests::RejectsMalformedOrIncompleteRecords()
    {
        const auto unterminated = ParseRepoStatus("? file", 10);
        VERIFY_ARE_EQUAL(RepoStatusParseError::MissingRecordTerminator, unterminated.error);

        static constexpr char malformedHeader[]{ "# branch.ab +x -1\0" };
        const auto badHeader = ParseRepoStatus(std::string_view{ malformedHeader, sizeof(malformedHeader) - 1 }, 10);
        VERIFY_ARE_EQUAL(RepoStatusParseError::MalformedHeader, badHeader.error);

        static constexpr char missingSource[]{
            "2 R. N... 100644 100644 100644 aaaaaaa bbbbbbb R100 renamed.txt\0"
        };
        const auto badRename = ParseRepoStatus(std::string_view{ missingSource, sizeof(missingSource) - 1 }, 10);
        VERIFY_ARE_EQUAL(RepoStatusParseError::MissingRenameSource, badRename.error);
    }
}
