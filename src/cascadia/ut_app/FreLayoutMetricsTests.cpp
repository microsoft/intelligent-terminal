// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"
#include "../TerminalApp/FreLayoutMetrics.h"

using namespace WEX::TestExecution;

namespace TerminalAppUnitTests
{
    class FreLayoutMetricsTests
    {
        TEST_CLASS(FreLayoutMetricsTests);
        TEST_METHOD(DefaultWelcomeLayoutMatchesFigma);
        TEST_METHOD(MinimumWelcomeLayoutMatchesFigma);
        TEST_METHOD(WelcomeTypographyAndCardContentMatchFigma);
        TEST_METHOD(FreDefaultWindowSizeHonorsLaunchOverrides);
        TEST_METHOD(FreMinimumWindowSizeMatchesFigma);
        TEST_METHOD(FreWindowSizeFitsMonitorWorkArea);
        TEST_METHOD(FreCardCornerRadiusScalesWithLaunchWindow);
        TEST_METHOD(WelcomeLayoutSwitchesToScrollingBelowWideBreakpoint);
        TEST_METHOD(WelcomeLayoutScrollsWhenWideWindowIsTooShort);
    };

    void FreLayoutMetricsTests::DefaultWelcomeLayoutMatchesFigma()
    {
        const auto layout = winrt::TerminalApp::implementation::ResolveFreWelcomeLayout(1130.0, 631.0);

        VERIFY_ARE_EQUAL(1032.0, layout.cardsWidth);
        VERIFY_ARE_EQUAL(510.0, layout.cardWidth);
        VERIFY_ARE_EQUAL(12.0, layout.cardSpacing);
        VERIFY_ARE_EQUAL(80.0, layout.footerHeight);
        VERIFY_ARE_EQUAL(120.0, layout.buttonWidth);
        VERIFY_ARE_EQUAL(32.0, layout.buttonHeight);
        VERIFY_ARE_EQUAL(40.0, layout.titlebarHeight);
        VERIFY_ARE_EQUAL(631.0, layout.contentHeight);
        VERIFY_ARE_EQUAL(47.0, layout.headerTop);
        VERIFY_ARE_EQUAL(87.0, layout.headerWindowTop);
        VERIFY_ARE_EQUAL(113.0, layout.headerHeight);
        VERIFY_ARE_EQUAL(24.0, layout.headerToCardsSpacing);
        VERIFY_ARE_EQUAL(184.0, layout.cardsTop);
        VERIFY_ARE_EQUAL(224.0, layout.cardsWindowTop);
        VERIFY_ARE_EQUAL(554.0, layout.scrollViewportBottom);
        VERIFY_ARE_EQUAL(594.0, layout.scrollWindowBottom);
        VERIFY_ARE_EQUAL(551.0, layout.footerTop);
        VERIFY_ARE_EQUAL(591.0, layout.footerWindowTop);
        VERIFY_ARE_EQUAL(3.0, layout.footerOverlap);
    }

    void FreLayoutMetricsTests::MinimumWelcomeLayoutMatchesFigma()
    {
        const auto layout = winrt::TerminalApp::implementation::ResolveFreWelcomeLayout(711.0, 631.0);

        VERIFY_ARE_EQUAL(619.0, layout.cardsWidth);
        VERIFY_ARE_EQUAL(303.5, layout.cardWidth);
        VERIFY_ARE_EQUAL(12.0, layout.cardSpacing);
        VERIFY_ARE_EQUAL(46.0, layout.horizontalPadding);
        VERIFY_ARE_EQUAL(370.0, layout.cardMinHeight);
        VERIFY_ARE_EQUAL(80.0, layout.footerHeight);
    }

    void FreLayoutMetricsTests::WelcomeTypographyAndCardContentMatchFigma()
    {
        const auto layout = winrt::TerminalApp::implementation::ResolveFreWelcomeLayout(1130.0);

        VERIFY_ARE_EQUAL(28.0, layout.headerTitleFontSize);
        VERIFY_ARE_EQUAL(36.0, layout.headerTitleLineHeight);
        VERIFY_ARE_EQUAL(16.0, layout.headerSubtitleFontSize);
        VERIFY_ARE_EQUAL(22.0, layout.headerSubtitleLineHeight);
        VERIFY_ARE_EQUAL(24.0, layout.headerTextSpacing);
        VERIFY_ARE_EQUAL(20.0, layout.cardHorizontalPadding);
        VERIFY_ARE_EQUAL(24.0, layout.cardVerticalPadding);
        VERIFY_ARE_EQUAL(214.0, layout.cardImageHeight);
        VERIFY_ARE_EQUAL(465.0, layout.cardImageMaxWidth);
        VERIFY_ARE_EQUAL(10.0, layout.cardImageToTextSpacing);
        VERIFY_ARE_EQUAL(20.0, layout.cardTitleFontSize);
        VERIFY_ARE_EQUAL(28.0, layout.cardTitleLineHeight);
        VERIFY_ARE_EQUAL(14.0, layout.cardDescriptionFontSize);
        VERIFY_ARE_EQUAL(20.0, layout.cardDescriptionLineHeight);
        VERIFY_ARE_EQUAL(10.0, layout.cardTextSpacing);
        VERIFY_ARE_EQUAL(0, layout.autofixCardColumn);
        VERIFY_ARE_EQUAL(2, layout.sidebarCardColumn);
    }

    void FreLayoutMetricsTests::FreDefaultWindowSizeHonorsLaunchOverrides()
    {
        const auto defaultSize = winrt::TerminalApp::implementation::ResolveFreDefaultWindowSize(1.0);
        const auto scaledSize = winrt::TerminalApp::implementation::ResolveFreDefaultWindowSize(1.5);
        const auto hostedTitlebarSize = winrt::TerminalApp::implementation::ResolveFreDefaultWindowSize(1.0, true);

        VERIFY_ARE_EQUAL(1130.0, defaultSize.width);
        VERIFY_ARE_EQUAL(631.0, defaultSize.height);
        VERIFY_ARE_EQUAL(1695.0, scaledSize.width);
        VERIFY_ARE_EQUAL(946.5, scaledSize.height);
        VERIFY_ARE_EQUAL(671.0, hostedTitlebarSize.height);

        VERIFY_IS_TRUE(winrt::TerminalApp::implementation::ShouldUseFreDefaultWindowSize(true, false, false, false, false, true));
        VERIFY_IS_FALSE(winrt::TerminalApp::implementation::ShouldUseFreDefaultWindowSize(false, false, false, false, false, true));
        VERIFY_IS_FALSE(winrt::TerminalApp::implementation::ShouldUseFreDefaultWindowSize(true, true, false, false, false, true));
        VERIFY_IS_TRUE(winrt::TerminalApp::implementation::ShouldUseFreDefaultWindowSize(true, false, true, false, false, true));
        VERIFY_IS_FALSE(winrt::TerminalApp::implementation::ShouldUseFreDefaultWindowSize(true, false, false, true, false, true));
        VERIFY_IS_FALSE(winrt::TerminalApp::implementation::ShouldUseFreDefaultWindowSize(true, false, false, false, true, true));
        VERIFY_IS_FALSE(winrt::TerminalApp::implementation::ShouldUseFreDefaultWindowSize(true, false, false, false, false, false));
    }

    void FreLayoutMetricsTests::FreMinimumWindowSizeMatchesFigma()
    {
        VERIFY_ARE_EQUAL(711.0, winrt::TerminalApp::implementation::ClampFreMinimumWindowDimension(true, 460.0));
        VERIFY_ARE_EQUAL(900.0, winrt::TerminalApp::implementation::ClampFreMinimumWindowDimension(true, 900.0));
        VERIFY_ARE_EQUAL(631.0, winrt::TerminalApp::implementation::ClampFreMinimumWindowDimension(false, 300.0));
        VERIFY_ARE_EQUAL(800.0, winrt::TerminalApp::implementation::ClampFreMinimumWindowDimension(false, 800.0));
    }

    void FreLayoutMetricsTests::FreWindowSizeFitsMonitorWorkArea()
    {
        constexpr auto constrained = winrt::TerminalApp::implementation::ClampFreWindowSizeToWorkArea(
            { 2260.0, 1342.0 },
            { 1920.0, 1040.0 });
        static_assert(constrained.width > 1751.0 && constrained.width < 1752.0);

        VERIFY_ARE_EQUAL(1040.0, constrained.height);
        VERIFY_ARE_EQUAL(1752l, lrint(constrained.width));
        VERIFY_ARE_EQUAL(1040l, lrint(constrained.height));
        VERIFY_IS_TRUE(std::abs(
                           (constrained.width / constrained.height) -
                           (2260.0 / 1342.0)) <
                       0.000001);

        const auto position = winrt::TerminalApp::implementation::ClampFreWindowPositionToWorkArea(
            { 100.0, 100.0 },
            constrained,
            { 0.0, 0.0 },
            { 1920.0, 1040.0 });
        VERIFY_ARE_EQUAL(0.0, position.x);
        VERIFY_ARE_EQUAL(0.0, position.y);
    }

    void FreLayoutMetricsTests::FreCardCornerRadiusScalesWithLaunchWindow()
    {
        VERIFY_ARE_EQUAL(7.0, winrt::TerminalApp::implementation::ResolveFreCardCornerRadius(1130.0, 631.0));
        VERIFY_ARE_EQUAL(7.0, winrt::TerminalApp::implementation::ResolveFreCardCornerRadius(1400.0, 800.0));
        VERIFY_ARE_EQUAL(0.0, winrt::TerminalApp::implementation::ResolveFreCardCornerRadius(0.0, 0.0));

        const auto constrainedRadius = winrt::TerminalApp::implementation::ResolveFreCardCornerRadius(990.0, 548.0);
        VERIFY_IS_TRUE(std::abs(constrainedRadius - 6.13) < 0.01);
    }

    void FreLayoutMetricsTests::WelcomeLayoutSwitchesToScrollingBelowWideBreakpoint()
    {
        const auto wide = winrt::TerminalApp::implementation::ResolveFreWelcomeLayout(1020.0);
        const auto compact = winrt::TerminalApp::implementation::ResolveFreWelcomeLayout(1019.0);

        VERIFY_ARE_EQUAL(12.0, wide.cardSpacing);
        VERIFY_ARE_EQUAL(12.0, compact.cardSpacing);
        VERIFY_ARE_EQUAL((wide.cardsWidth - wide.cardSpacing) / 2.0, wide.cardWidth);
        VERIFY_ARE_EQUAL((compact.cardsWidth - compact.cardSpacing) / 2.0, compact.cardWidth);
    }

    void FreLayoutMetricsTests::WelcomeLayoutScrollsWhenWideWindowIsTooShort()
    {
        const auto layout = winrt::TerminalApp::implementation::ResolveFreWelcomeLayout(1130.0, 630.0);

        VERIFY_ARE_EQUAL(370.0, layout.cardMinHeight);
    }
}
