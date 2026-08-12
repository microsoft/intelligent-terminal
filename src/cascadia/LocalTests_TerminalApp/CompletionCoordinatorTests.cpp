// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "../TerminalApp/CompletionCoordinator.h"

#include <WexTestClass.h>

using namespace WEX::TestExecution;
using namespace winrt::TerminalApp::implementation;

namespace TerminalAppLocalTests
{
    class CompletionCoordinatorTests
    {
        TEST_CLASS(CompletionCoordinatorTests);

        TEST_METHOD(RejectsStaleGeneration);
        TEST_METHOD(RejectsStaleResponseForSameTarget);
        TEST_METHOD(DefersLatestRequestWhileResponsePending);
        TEST_METHOD(AllowsIndependentTargets);
        TEST_METHOD(RecoversFromFailedDispatch);
        TEST_METHOD(RemovesClosedTarget);
        TEST_METHOD(RejectsWrongTarget);
        TEST_METHOD(InvalidatesScheduledAndDisplayedRequests);
        TEST_METHOD(ClassifiesInputEvents);
    };

    void CompletionCoordinatorTests::RejectsStaleGeneration()
    {
        CompletionRequestState state;
        const auto first = state.Schedule(1);
        const auto second = state.Schedule(1);

        VERIFY_IS_FALSE(state.Dispatch(first, 1));
        VERIFY_IS_TRUE(state.Dispatch(second, 1));
        VERIFY_IS_TRUE(state.AcceptResponse(1));
    }

    void CompletionCoordinatorTests::RejectsStaleResponseForSameTarget()
    {
        CompletionRequestState state;
        const auto first = state.Schedule(1);
        VERIFY_IS_TRUE(state.Dispatch(first, 1));

        const auto second = state.Schedule(1);
        VERIFY_IS_FALSE(state.Dispatch(second, 1));

        VERIFY_IS_FALSE(state.AcceptResponse(1));
        VERIFY_IS_TRUE(state.Dispatch(second, 1));
        VERIFY_IS_TRUE(state.AcceptResponse(1));
    }

    void CompletionCoordinatorTests::DefersLatestRequestWhileResponsePending()
    {
        CompletionRequestState state;
        const auto first = state.Schedule(1);
        VERIFY_IS_TRUE(state.Dispatch(first, 1));
        VERIFY_IS_TRUE(state.HasPendingResponse(1));

        const auto second = state.Schedule(1);
        VERIFY_IS_FALSE(state.AcceptResponse(1));
        VERIFY_IS_FALSE(state.HasPendingResponse(1));
        VERIFY_IS_TRUE(state.Dispatch(second, 1));
    }

    void CompletionCoordinatorTests::AllowsIndependentTargets()
    {
        CompletionRequestState state;
        const auto first = state.Schedule(1);
        VERIFY_IS_TRUE(state.Dispatch(first, 1));

        const auto second = state.Schedule(2);
        VERIFY_IS_TRUE(state.Dispatch(second, 2));
        VERIFY_IS_TRUE(state.HasPendingResponse(1));
        VERIFY_IS_TRUE(state.HasPendingResponse(2));
    }

    void CompletionCoordinatorTests::RecoversFromFailedDispatch()
    {
        CompletionRequestState state;
        const auto first = state.Schedule(1);
        VERIFY_IS_TRUE(state.Dispatch(first, 1));

        const auto second = state.Schedule(1);
        VERIFY_IS_TRUE(state.ExpireResponse(first, 1));
        VERIFY_IS_FALSE(state.HasPendingResponse(1));
        VERIFY_IS_TRUE(state.Dispatch(second, 1));
        VERIFY_IS_TRUE(state.AcceptResponse(1));
    }

    void CompletionCoordinatorTests::RemovesClosedTarget()
    {
        CompletionRequestState state;
        const auto generation = state.Schedule(1);
        VERIFY_IS_TRUE(state.Dispatch(generation, 1));
        VERIFY_IS_TRUE(state.RemoveTarget(1));
        VERIFY_IS_FALSE(state.HasPendingResponse(1));
        VERIFY_ARE_EQUAL(CompletionRequestState::Phase::Idle, state.CurrentPhase());
    }

    void CompletionCoordinatorTests::RejectsWrongTarget()
    {
        CompletionRequestState state;
        const auto generation = state.Schedule(1);

        VERIFY_IS_FALSE(state.Dispatch(generation, 2));
        VERIFY_IS_TRUE(state.Dispatch(generation, 1));
        VERIFY_IS_FALSE(state.AcceptResponse(2));
        VERIFY_IS_TRUE(state.AcceptResponse(1));
    }

    void CompletionCoordinatorTests::InvalidatesScheduledAndDisplayedRequests()
    {
        CompletionRequestState state;
        state.Schedule(1);
        VERIFY_IS_TRUE(state.Invalidate(1));
        VERIFY_ARE_EQUAL(CompletionRequestState::Phase::Idle, state.CurrentPhase());

        const auto generation = state.Schedule(2);
        VERIFY_IS_TRUE(state.Dispatch(generation, 2));
        VERIFY_IS_TRUE(state.AcceptResponse(2));
        VERIFY_IS_TRUE(state.Invalidate(2));
        VERIFY_ARE_EQUAL(CompletionRequestState::Phase::Idle, state.CurrentPhase());
    }

    void CompletionCoordinatorTests::ClassifiesInputEvents()
    {
        VERIFY_ARE_EQUAL(CompletionInputAction::Ignore, CompletionInputClassifier::ClassifyKey(VK_BACK, false));
        VERIFY_ARE_EQUAL(CompletionInputAction::Request, CompletionInputClassifier::ClassifyKey(VK_BACK, true));
        VERIFY_ARE_EQUAL(CompletionInputAction::Request, CompletionInputClassifier::ClassifyKey(VK_DELETE, true));
        VERIFY_ARE_EQUAL(CompletionInputAction::Invalidate, CompletionInputClassifier::ClassifyKey(VK_RETURN, true));

        VERIFY_ARE_EQUAL(CompletionInputAction::Request, CompletionInputClassifier::ClassifyCharacter(L'a'));
        VERIFY_ARE_EQUAL(CompletionInputAction::Invalidate, CompletionInputClassifier::ClassifyCharacter(L' '));
        VERIFY_ARE_EQUAL(CompletionInputAction::Invalidate, CompletionInputClassifier::ClassifyCharacter(L'\r'));

        VERIFY_ARE_EQUAL(CompletionInputAction::Ignore, CompletionInputClassifier::ClassifyString(L""));
        VERIFY_ARE_EQUAL(CompletionInputAction::Ignore, CompletionInputClassifier::ClassifyString(L"\x1b[24~b"));
        VERIFY_ARE_EQUAL(CompletionInputAction::Ignore, CompletionInputClassifier::ClassifyString(L"\x1b[24~cfoo"));
        VERIFY_ARE_EQUAL(CompletionInputAction::Request, CompletionInputClassifier::ClassifyString(L"Get-ChildItem"));
        VERIFY_ARE_EQUAL(CompletionInputAction::Invalidate, CompletionInputClassifier::ClassifyString(L"Get-ChildItem "));
        VERIFY_ARE_EQUAL(CompletionInputAction::Invalidate, CompletionInputClassifier::ClassifyString(L"Get-ChildItem\r"));
        VERIFY_ARE_EQUAL(CompletionInputAction::Invalidate, CompletionInputClassifier::ClassifyString(L"echo one\necho two"));
        VERIFY_ARE_EQUAL(CompletionInputAction::Invalidate, CompletionInputClassifier::ClassifyString(L"\x1b[A"));
    }
}
