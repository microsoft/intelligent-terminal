// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "TerminalPage.h"

#include "../inc/BashShellIntegration.h"
#include "../inc/PowerShellShellIntegration.h"
#include "BashProfileAnalyzer.h"
#include "PowerShellProfileAnalyzer.h"
#include "ShellIntegrationInvocation.h"
#include "ShellIntegrationProfileHealthService.h"
#include "WslProfileHealthResolver.h"

using namespace winrt;
using namespace winrt::Microsoft::Terminal::TerminalConnection;
using namespace winrt::Windows::UI::Core;
namespace MUX = winrt::Microsoft::UI::Xaml;
namespace Health = ::Microsoft::Terminal::ShellIntegration::Health;
namespace Invocation = ::Microsoft::Terminal::ShellIntegration::Invocation;
namespace Bash = ::Microsoft::Terminal::ShellIntegration::Bash;
namespace Powershell = ::Microsoft::Terminal::ShellIntegration::Powershell;
namespace ShellIntegration = ::Microsoft::Terminal::ShellIntegration;
namespace WslProfileHealth = ::Microsoft::Terminal::ShellIntegration::Wsl::ProfileHealth;

namespace
{
    std::optional<std::wstring> _RootProcessImage(const ConptyConnection& connection)
    {
        const auto process = reinterpret_cast<HANDLE>(connection.RootProcessHandle());
        if (!process)
        {
            return std::nullopt;
        }

        std::wstring image(32768, L'\0');
        DWORD length = static_cast<DWORD>(image.size());
        if (!QueryFullProcessImageNameW(process, 0, image.data(), &length) || length == 0)
        {
            return std::nullopt;
        }
        image.resize(length);
        return image;
    }

    bool _IsGitForWindowsBash(const std::wstring_view imagePath)
    {
        if (imagePath.starts_with(L"\\\\"))
        {
            return false;
        }

        const std::filesystem::path image{ imagePath };
        const auto bin = image.parent_path();
        if (_wcsicmp(image.filename().c_str(), L"bash.exe") != 0 ||
            _wcsicmp(bin.filename().c_str(), L"bin") != 0)
        {
            return false;
        }

        const auto parent = bin.parent_path();
        const auto root = _wcsicmp(parent.filename().c_str(), L"usr") == 0 ?
                              parent.parent_path() :
                              parent;
        const auto git = root / L"cmd" / L"git.exe";
        const auto runtime = root / L"usr" / L"bin" / L"msys-2.0.dll";
        const auto isFile = [](const std::filesystem::path& path) {
            const auto attributes = GetFileAttributesW(path.c_str());
            return attributes != INVALID_FILE_ATTRIBUTES &&
                   WI_IsFlagClear(attributes, FILE_ATTRIBUTE_DIRECTORY);
        };
        return isFile(git) && isFile(runtime);
    }

    safe_void_coroutine _OpenShellProfile(std::wstring path)
    {
        co_await winrt::resume_background();

        const auto result = ShellExecuteW(nullptr, L"open", path.c_str(), nullptr, nullptr, SW_SHOWNORMAL);
        if (reinterpret_cast<INT_PTR>(result) <= 32)
        {
            ShellExecuteW(nullptr, L"open", L"notepad.exe", path.c_str(), nullptr, SW_SHOWNORMAL);
        }
    }
}

namespace winrt::TerminalApp::implementation
{
    void TerminalPage::_BindShellIntegrationProfileHealth(
        const ITerminalConnection& connection,
        const bool hasCustomHome)
    {
        if (!ShellIntegrationProfileHealthService::Instance().Enabled())
        {
            return;
        }

        const auto conpty = connection.try_as<ConptyConnection>();
        if (!conpty)
        {
            return;
        }

        const std::wstring commandline{ conpty.Commandline() };
        auto handled = std::make_shared<std::atomic<bool>>(false);
        conpty.StateChanged(
            [weakThis = get_weak(), commandline, hasCustomHome, handled](const auto& sender, const auto&) {
                const auto launched = sender.template try_as<ConptyConnection>();
                if (!launched ||
                    launched.State() != ConnectionState::Connected ||
                    handled->exchange(true, std::memory_order_acq_rel))
                {
                    return;
                }

                const auto image = _RootProcessImage(launched);
                if (!image)
                {
                    return;
                }

                const auto classification = Invocation::Classify(commandline, std::wstring_view{ *image });
                const bool resolvableWslIdentity =
                    classification.shellKind == Health::ShellKind::WslBash &&
                    classification.reason == Invocation::Reason::WslIdentityRequired;
                if ((!classification.eligible && !resolvableWslIdentity) || !classification.shellKind)
                {
                    return;
                }

                auto page = weakThis.get();
                if (!page)
                {
                    return;
                }

                Health::TargetKey target;
                target.shell = *classification.shellKind;
                switch (*classification.shellKind)
                {
                case Health::ShellKind::PowerShell:
                    target.syntax = Health::ProfileSyntax::PowerShell;
                    target.profilePath = Powershell::DiscoverProfilePath(ShellIntegration::Target::Pwsh);
                    target.hostPath = *image;
                    break;
                case Health::ShellKind::WindowsPowerShell:
                    target.syntax = Health::ProfileSyntax::PowerShell;
                    target.profilePath = Powershell::DiscoverProfilePath(ShellIntegration::Target::WindowsPowerShell);
                    target.hostPath = *image;
                    break;
                case Health::ShellKind::GitBash:
                    if (hasCustomHome || !_IsGitForWindowsBash(*image))
                    {
                        return;
                    }
                    target.syntax = Health::ProfileSyntax::Bash;
                    target.profilePath = Bash::DiscoverProfilePath();
                    break;
                case Health::ShellKind::WslBash:
                    page->_ResolveWslShellIntegrationProfileHealth(commandline, *image);
                    return;
                }

                if (!target.profilePath.empty())
                {
                    page->_RequestShellIntegrationProfileHealth(std::move(target), false);
                }
            });
    }

    void TerminalPage::_RequestShellIntegrationProfileHealth(Health::TargetKey target, const bool force)
    {
        const auto generation = _shellIntegrationProfileHealthGeneration;
        ShellIntegrationProfileHealthService::Instance().Request(
            std::move(target),
            generation,
            [](const Health::TargetKey& target, const std::string_view profile) {
                if (target.syntax == Health::ProfileSyntax::PowerShell)
                {
                    return Powershell::ProfileAnalyzer::Analyze(target.hostPath, profile);
                }
                return Bash::ProfileAnalyzer::Analyze(profile);
            },
            [weakThis = get_weak(), generation](const Health::Result& result) {
                if (const auto page = weakThis.get())
                {
                    try
                    {
                        page->Dispatcher().RunAsync(
                            CoreDispatcherPriority::Normal,
                            [weakThis, generation, result]() {
                                if (const auto page = weakThis.get())
                                {
                                    const auto& service = ShellIntegrationProfileHealthService::Instance();
                                    if (page->_shellIntegrationProfileHealthGeneration == generation &&
                                        service.Enabled() &&
                                        service.Generation() == generation)
                                    {
                                        page->_ApplyShellIntegrationProfileHealthResult(result);
                                    }
                                }
                            });
                    }
                    catch (...)
                    {
                        LOG_CAUGHT_EXCEPTION();
                    }
                }
            },
            force);
    }

    safe_void_coroutine TerminalPage::_ResolveWslShellIntegrationProfileHealth(
        std::wstring commandline,
        std::wstring actualRootImagePath)
    {
        const auto weakThis = get_weak();
        const auto dispatcher = Dispatcher();
        co_await winrt::resume_background();

        const auto classification = Invocation::Classify(commandline, std::wstring_view{ actualRootImagePath });
        auto resolution = WslProfileHealth::ResolveTarget(commandline, classification);
        if (!resolution)
        {
            co_return;
        }

        co_await winrt::resume_foreground(dispatcher);
        if (const auto page = weakThis.get())
        {
            page->_RequestShellIntegrationProfileHealth(std::move(*resolution.target), false);
        }
    }

    void TerminalPage::_ApplyShellIntegrationProfileHealthResult(const Health::Result& result)
    {
        if (!result.analysis.ShouldNotify())
        {
            std::erase_if(
                _pendingShellIntegrationProfileHealthWarnings,
                [&](const auto& pending) { return pending.target == result.target; });
            if (_shellIntegrationProfileHealthWarning &&
                _shellIntegrationProfileHealthWarning->target == result.target)
            {
                _shellIntegrationProfileHealthWarning.reset();
                _ShowNextShellIntegrationProfileHealthWarning();
            }
            return;
        }

        const auto dismissed = std::find_if(
            _dismissedShellIntegrationProfileHealthWarnings.begin(),
            _dismissedShellIntegrationProfileHealthWarnings.end(),
            [&](const auto& value) {
                return value.first == result.target && value.second == result.fingerprint;
            });
        if (dismissed != _dismissedShellIntegrationProfileHealthWarnings.end())
        {
            return;
        }

        if (_shellIntegrationProfileHealthWarning)
        {
            if (_shellIntegrationProfileHealthWarning->target == result.target)
            {
                _shellIntegrationProfileHealthWarning = result;
                return;
            }

            const auto pending = std::find_if(
                _pendingShellIntegrationProfileHealthWarnings.begin(),
                _pendingShellIntegrationProfileHealthWarnings.end(),
                [&](const auto& value) { return value.target == result.target; });
            if (pending == _pendingShellIntegrationProfileHealthWarnings.end())
            {
                _pendingShellIntegrationProfileHealthWarnings.emplace_back(result);
            }
            else
            {
                *pending = result;
            }
            return;
        }

        _shellIntegrationProfileHealthWarning = result;
        if (const auto infoBar = FindName(L"ShellIntegrationProfileHealthInfoBar").try_as<MUX::Controls::InfoBar>())
        {
            infoBar.IsOpen(true);
        }
    }

    void TerminalPage::_ShowNextShellIntegrationProfileHealthWarning()
    {
        if (!_pendingShellIntegrationProfileHealthWarnings.empty())
        {
            _shellIntegrationProfileHealthWarning =
                std::move(_pendingShellIntegrationProfileHealthWarnings.front());
            _pendingShellIntegrationProfileHealthWarnings.erase(
                _pendingShellIntegrationProfileHealthWarnings.begin());
        }

        if (const auto infoBar = FindName(L"ShellIntegrationProfileHealthInfoBar").try_as<MUX::Controls::InfoBar>())
        {
            infoBar.IsOpen(_shellIntegrationProfileHealthWarning.has_value());
        }
    }

    void TerminalPage::_ClearShellIntegrationProfileHealthWarning()
    {
        _shellIntegrationProfileHealthWarning.reset();
        _pendingShellIntegrationProfileHealthWarnings.clear();
        if (const auto infoBar = FindName(L"ShellIntegrationProfileHealthInfoBar").try_as<MUX::Controls::InfoBar>())
        {
            infoBar.IsOpen(false);
        }
    }

    void TerminalPage::_ShellIntegrationProfileHealthCloseHandler(const IInspectable&, const IInspectable&)
    {
        if (_shellIntegrationProfileHealthWarning)
        {
            _dismissedShellIntegrationProfileHealthWarnings.emplace_back(
                _shellIntegrationProfileHealthWarning->target,
                _shellIntegrationProfileHealthWarning->fingerprint);
        }
        _shellIntegrationProfileHealthWarning.reset();
        _ShowNextShellIntegrationProfileHealthWarning();
    }

    void TerminalPage::_ShellIntegrationProfileHealthOpenHandler(const IInspectable&, const IInspectable&)
    {
        if (_shellIntegrationProfileHealthWarning)
        {
            _OpenShellProfile(_shellIntegrationProfileHealthWarning->target.profilePath);
        }
    }

    void TerminalPage::_ShellIntegrationProfileHealthRecheckHandler(const IInspectable&, const IInspectable&)
    {
        if (_shellIntegrationProfileHealthWarning)
        {
            _RequestShellIntegrationProfileHealth(_shellIntegrationProfileHealthWarning->target, true);
        }
    }
}
