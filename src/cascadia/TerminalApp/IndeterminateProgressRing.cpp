// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include <winrt/Windows.UI.Xaml.Interop.h>
#include <winrt/Windows.UI.Xaml.Hosting.h>
#include "IndeterminateProgressRing.h"

#include "IndeterminateProgressRing.g.cpp"
#include "IndeterminateProgressRingAutomationPeer.g.cpp"

using namespace winrt;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Media;
using namespace winrt::Windows::UI::Composition;

namespace winrt::TerminalApp::implementation
{
    DependencyProperty IndeterminateProgressRing::_IsActiveProperty{ nullptr };

    IndeterminateProgressRing::IndeterminateProgressRing()
    {
        static const auto registered = [] {
            _IsActiveProperty = DependencyProperty::Register(
                L"IsActive",
                xaml_typename<bool>(),
                xaml_typename<TerminalApp::IndeterminateProgressRing>(),
                PropertyMetadata{ box_value(false), [](const DependencyObject& sender, const DependencyPropertyChangedEventArgs&) {
                                     get_self<IndeterminateProgressRing>(sender.as<TerminalApp::IndeterminateProgressRing>())->_UpdateAnimation();
                                 } });
            return true;
        }();
        (void)registered;

        RegisterPropertyChangedCallback(Controls::Control::ForegroundProperty(), [weakThis = get_weak()](auto&&, auto&&) {
            if (const auto self = weakThis.get())
            {
                self->_UpdateForeground();
            }
        });
        Loaded([weakThis = get_weak()](auto&&, auto&&) {
            if (const auto self = weakThis.get())
            {
                // A recycled view needs a clock attached to its current visual tree.
                self->_ObserveVisibility();
                self->_UpdateAnimation();
            }
        });
        Unloaded([weakThis = get_weak()](auto&&, auto&&) {
            if (const auto self = weakThis.get())
            {
                if (self->IsLoaded())
                {
                    self->_ObserveVisibility();
                    self->_UpdateAnimation();
                }
                else
                {
                    self->_StopAnimation();
                    self->_ClearVisibilityObservers();
                }
            }
        });
        EffectiveViewportChanged([weakThis = get_weak()](auto&&, auto&&) {
            if (const auto self = weakThis.get())
            {
                self->_ObserveVisibility();
                self->_UpdateAnimation();
            }
        });
    }

    IndeterminateProgressRing::~IndeterminateProgressRing()
    {
        try
        {
            _StopAnimation();
            _ClearVisibilityObservers();
            if (_foreground)
            {
                _foreground.UnregisterPropertyChangedCallback(SolidColorBrush::ColorProperty(), _foregroundToken);
            }
        }
        CATCH_LOG();
    }

    void IndeterminateProgressRing::OnApplyTemplate()
    {
        _StopAnimation();
        _visual = nullptr;
        _strokeBrush = nullptr;
        if (const auto view = GetTemplateChild(L"SpinnerView").try_as<FrameworkElement>())
        {
            const auto compositor = Windows::UI::Xaml::Hosting::ElementCompositionPreview::GetElementVisual(view).Compositor();
            const auto geometry = compositor.CreateEllipseGeometry();
            geometry.Center({ 7.5f, 7.5f });
            geometry.Radius({ 6.0f, 6.0f });
            geometry.TrimEnd(0.75f);
            _strokeBrush = compositor.CreateColorBrush();
            const auto arc = compositor.CreateSpriteShape(geometry);
            arc.StrokeThickness(1.5f);
            arc.StrokeBrush(_strokeBrush);
            _visual = compositor.CreateShapeVisual();
            _visual.Size({ 15.0f, 15.0f });
            _visual.CenterPoint({ 7.5f, 7.5f, 0.0f });
            _visual.Shapes().Append(arc);
            Windows::UI::Xaml::Hosting::ElementCompositionPreview::SetElementChildVisual(view, _visual);
            _UpdateForeground();
        }
        _UpdateAnimation();
    }

    Automation::Peers::AutomationPeer IndeterminateProgressRing::OnCreateAutomationPeer()
    {
        return make<IndeterminateProgressRingAutomationPeer>(*this);
    }

    void IndeterminateProgressRing::_ObserveVisibility()
    {
        _ClearVisibilityObservers();
        // Visibility is not inherited as a dependency property. Observe the attached
        // visual ancestry, not the progress model, and release it on every unload.
        for (DependencyObject current = *this; current; current = VisualTreeHelper::GetParent(current))
        {
            if (const auto element = current.try_as<UIElement>())
            {
                const auto token = element.RegisterPropertyChangedCallback(UIElement::VisibilityProperty(), [weakThis = get_weak()](auto&&, auto&&) {
                    if (const auto self = weakThis.get())
                    {
                        self->_UpdateAnimation();
                    }
                });
                _visibilitySubscriptions.push_back({ make_weak(element), token });
            }
        }
    }

    void IndeterminateProgressRing::_ClearVisibilityObservers()
    {
        for (const auto& subscription : _visibilitySubscriptions)
        {
            if (const auto element = subscription.element.get())
            {
                element.UnregisterPropertyChangedCallback(UIElement::VisibilityProperty(), subscription.token);
            }
        }
        _visibilitySubscriptions.clear();
    }

    void IndeterminateProgressRing::_UpdateAnimation()
    {
        auto visible = IsLoaded() && IsActive() && _visual;
        if (visible)
        {
            for (DependencyObject current = *this; current; current = VisualTreeHelper::GetParent(current))
            {
                if (const auto element = current.try_as<UIElement>(); element && element.Visibility() != Visibility::Visible)
                {
                    visible = false;
                    break;
                }
            }
        }
        if (visible && !_animation)
        {
            _animation = _visual.Compositor().CreateScalarKeyFrameAnimation();
            _animation.InsertKeyFrame(0.0f, 0.0f);
            _animation.InsertKeyFrame(1.0f, 360.0f);
            _animation.Duration(std::chrono::seconds{ 1 });
            _animation.IterationBehavior(AnimationIterationBehavior::Forever);
            _visual.StartAnimation(L"RotationAngleInDegrees", _animation);
        }
        else if (!visible)
        {
            _StopAnimation();
        }
    }

    void IndeterminateProgressRing::_StopAnimation()
    {
        if (_animation)
        {
            _visual.StopAnimation(L"RotationAngleInDegrees");
            _visual.RotationAngleInDegrees(0.0f);
            _animation = nullptr;
        }
    }

    void IndeterminateProgressRing::_UpdateForeground()
    {
        const auto foreground = Foreground();
        const auto brush = foreground.try_as<SolidColorBrush>();
        if (_foreground != brush)
        {
            if (_foreground)
            {
                _foreground.UnregisterPropertyChangedCallback(SolidColorBrush::ColorProperty(), _foregroundToken);
            }
            _foreground = brush;
            if (_foreground)
            {
                _foregroundToken = _foreground.RegisterPropertyChangedCallback(SolidColorBrush::ColorProperty(), [weakThis = get_weak()](auto&&, auto&&) {
                    if (const auto self = weakThis.get())
                    {
                        self->_UpdateForeground();
                    }
                });
            }
        }
        if (_strokeBrush)
        {
            if (_foreground)
            {
                _strokeBrush.Color(_foreground.Color());
            }
            else
            {
                _strokeBrush.Color(Windows::UI::Colors::Transparent());
                if (foreground)
                {
                    LOG_HR(E_NOTIMPL);
                }
            }
        }
    }

    IndeterminateProgressRingAutomationPeer::IndeterminateProgressRingAutomationPeer(const TerminalApp::IndeterminateProgressRing& owner) :
        IndeterminateProgressRingAutomationPeerT<IndeterminateProgressRingAutomationPeer>(owner)
    {
    }

    winrt::hstring IndeterminateProgressRingAutomationPeer::GetClassNameCore() const
    {
        return L"ProgressRing";
    }

    Automation::Peers::AutomationControlType IndeterminateProgressRingAutomationPeer::GetAutomationControlTypeCore() const
    {
        return Automation::Peers::AutomationControlType::ProgressBar;
    }

}
