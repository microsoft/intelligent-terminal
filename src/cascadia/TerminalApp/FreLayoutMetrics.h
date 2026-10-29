// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <algorithm>

namespace winrt::TerminalApp::implementation
{
    struct FreWindowSize
    {
        double width{};
        double height{};
    };

    constexpr FreWindowSize ResolveFreDefaultWindowSize(const double scale) noexcept
    {
        return {
            1130.0 * scale,
            631.0 * scale,
        };
    }

    constexpr FreWindowSize ClampFreWindowSizeToWorkArea(const FreWindowSize requested,
                                                         const FreWindowSize workArea) noexcept
    {
        return {
            std::min(requested.width, workArea.width),
            std::min(requested.height, workArea.height),
        };
    }

    constexpr double ClampFreMinimumWindowDimension(const bool widthOrHeight,
                                                     const double dimension) noexcept
    {
        return std::max(dimension, widthOrHeight ? 711.0 : 631.0);
    }

    constexpr bool ShouldUseFreDefaultWindowSize(const bool freRequired,
                                                 const bool hasExplicitSize,
                                                 const bool /*hasPersistedSize*/,
                                                 const bool hasStartupKeptSize,
                                                 const bool hasContentBounds,
                                                 const bool isDefaultLaunchMode) noexcept
    {
        return freRequired &&
               !hasExplicitSize &&
               !hasStartupKeptSize &&
               !hasContentBounds &&
               isDefaultLaunchMode;
    }

    struct FreWelcomeLayout
    {
        double cardsWidth{};
        double cardWidth{};
        double horizontalPadding{};
        double cardSpacing{ 12.0 };
        double cardMinHeight{};
        double footerHeight{ 80.0 };
        double buttonWidth{ 120.0 };
        double buttonHeight{ 32.0 };
        double titlebarHeight{ 40.0 };
        double contentHeight{ 631.0 };
        double headerTop{ 47.0 };
        double headerWindowTop{ 87.0 };
        double headerHeight{ 113.0 };
        double headerToCardsSpacing{ 24.0 };
        double cardsTop{ 184.0 };
        double cardsWindowTop{ 224.0 };
        double scrollViewportBottom{ 554.0 };
        double scrollWindowBottom{ 594.0 };
        double footerTop{ 551.0 };
        double footerWindowTop{ 591.0 };
        double footerOverlap{ 3.0 };
        double headerTitleFontSize{ 28.0 };
        double headerTitleLineHeight{ 36.0 };
        double headerSubtitleFontSize{ 16.0 };
        double headerSubtitleLineHeight{ 22.0 };
        double headerTextSpacing{ 24.0 };
        double cardHorizontalPadding{ 20.0 };
        double cardVerticalPadding{ 24.0 };
        double cardImageHeight{ 214.0 };
        double cardImageMaxWidth{ 465.0 };
        double cardImageToTextSpacing{ 10.0 };
        double cardTitleFontSize{ 20.0 };
        double cardTitleLineHeight{ 28.0 };
        double cardDescriptionFontSize{ 14.0 };
        double cardDescriptionLineHeight{ 20.0 };
        double cardTextSpacing{ 10.0 };
        int autofixCardColumn{ 0 };
        int sidebarCardColumn{ 2 };
    };

    constexpr FreWelcomeLayout ResolveFreWelcomeLayout(const double viewportWidth,
                                                        const double /*viewportHeight*/ = 631.0) noexcept
    {
        constexpr double wideBreakpoint = 1020.0;
        constexpr double maximumCardsWidth = 1032.0;
        constexpr double wideHorizontalPadding = 48.0;
        constexpr double compactHorizontalPadding = 46.0;

        const bool compact = viewportWidth < wideBreakpoint;
        const double horizontalPadding = compact ? compactHorizontalPadding : wideHorizontalPadding;
        const double cardsWidth = std::max(0.0, std::min(maximumCardsWidth, viewportWidth - (horizontalPadding * 2.0)));

        FreWelcomeLayout result;
        result.cardsWidth = cardsWidth;
        result.cardWidth = std::max(0.0, (cardsWidth - result.cardSpacing) / 2.0);
        result.horizontalPadding = horizontalPadding;
        result.cardMinHeight = 370.0;
        return result;
    }
}
