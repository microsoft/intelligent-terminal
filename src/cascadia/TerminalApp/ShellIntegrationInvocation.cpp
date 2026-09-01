// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "ShellIntegrationInvocation.h"

#include <shellapi.h>
#include <vector>
#include <wil/resource.h>

using namespace Microsoft::Terminal::ShellIntegration::Health;

namespace Microsoft::Terminal::ShellIntegration::Invocation
{
    namespace
    {
        struct Root
        {
            std::wstring_view token;
            std::wstring_view leaf;
            bool absolute{ false };
        };

        enum class BashParseResult : uint8_t
        {
            Eligible,
            UnsupportedOption,
            NoRc,
            CustomRcFile,
            Command,
            ScriptOperand,
            LoginNoProfile,
            LoginProfileUnproven,
            PosixMode,
        };

        constexpr bool _IsSlash(const wchar_t c) noexcept
        {
            return c == L'\\' || c == L'/';
        }

        constexpr wchar_t _Fold(const wchar_t c) noexcept
        {
            return c >= L'A' && c <= L'Z' ? static_cast<wchar_t>(c + L'a' - L'A') : c;
        }

        bool _EqualsCi(const std::wstring_view left, const std::wstring_view right) noexcept
        {
            if (left.size() != right.size())
            {
                return false;
            }
            for (size_t i = 0; i < left.size(); ++i)
            {
                if (_Fold(left[i]) != _Fold(right[i]))
                {
                    return false;
                }
            }
            return true;
        }

        bool _StartsWithCi(const std::wstring_view value, const std::wstring_view prefix) noexcept
        {
            return value.size() >= prefix.size() && _EqualsCi(value.substr(0, prefix.size()), prefix);
        }

        bool _EqualsPathCiNormalized(std::wstring_view left, std::wstring_view right) noexcept
        {
            // The DOS device prefix does not change the path identity for the
            // forms this classifier accepts. Dot-segment resolution is
            // intentionally omitted: it would require filesystem semantics.
            if (_StartsWithCi(left, L"\\\\?\\"))
            {
                left.remove_prefix(4);
            }
            if (_StartsWithCi(right, L"\\\\?\\"))
            {
                right.remove_prefix(4);
            }
            if (left.size() != right.size())
            {
                return false;
            }
            for (size_t i = 0; i < left.size(); ++i)
            {
                const auto normalizedLeft = _IsSlash(left[i]) ? L'\\' : _Fold(left[i]);
                const auto normalizedRight = _IsSlash(right[i]) ? L'\\' : _Fold(right[i]);
                if (normalizedLeft != normalizedRight)
                {
                    return false;
                }
            }
            return true;
        }

        bool _IsAbsoluteWindowsPath(std::wstring_view path) noexcept
        {
            if (_StartsWithCi(path, L"\\\\?\\"))
            {
                path.remove_prefix(4);
            }
            if (path.size() >= 3 &&
                ((path[0] >= L'A' && path[0] <= L'Z') || (path[0] >= L'a' && path[0] <= L'z')) &&
                path[1] == L':' && _IsSlash(path[2]))
            {
                return true;
            }
            // A UNC root needs a server and share component. Do not treat
            // "\\" by itself as a root image.
            if (path.size() < 5 || !_IsSlash(path[0]) || !_IsSlash(path[1]))
            {
                return false;
            }
            const auto serverEnd = path.find_first_of(L"\\/", 2);
            return serverEnd != std::wstring_view::npos && serverEnd + 1 < path.size() &&
                   path.find_first_of(L"\\/", serverEnd + 1) != serverEnd + 1;
        }

        Root _RootOf(const std::wstring_view token) noexcept
        {
            const auto separator = token.find_last_of(L"\\/");
            return {
                token,
                separator == std::wstring_view::npos ? token : token.substr(separator + 1),
                _IsAbsoluteWindowsPath(token),
            };
        }

        bool _IsBareLeaf(const std::wstring_view token) noexcept
        {
            return !token.empty() && token.find_first_of(L"\\/:") == std::wstring_view::npos;
        }

        struct WindowsDirectory
        {
            static constexpr size_t Capacity{ MAX_PATH };
            wchar_t path[Capacity]{};
            size_t length{ 0 };
        };

        std::optional<WindowsDirectory> _GetWindowsDirectory() noexcept
        {
            WindowsDirectory directory;
            auto length = GetWindowsDirectoryW(directory.path, static_cast<UINT>(WindowsDirectory::Capacity));
            if (length == 0 || length >= WindowsDirectory::Capacity)
            {
                return std::nullopt;
            }
            while (length > 0 && _IsSlash(directory.path[length - 1]))
            {
                --length;
            }
            if (length == 0)
            {
                return std::nullopt;
            }
            directory.length = length;
            return directory;
        }

        bool _EqualsWindowsDirectoryPath(std::wstring_view path,
                                         const WindowsDirectory& windowsDirectory,
                                         const std::wstring_view suffix) noexcept
        {
            if (_StartsWithCi(path, L"\\\\?\\"))
            {
                path.remove_prefix(4);
            }
            if (path.size() != windowsDirectory.length + suffix.size())
            {
                return false;
            }
            for (size_t i = 0; i < path.size(); ++i)
            {
                const auto expected = i < windowsDirectory.length ? windowsDirectory.path[i] : suffix[i - windowsDirectory.length];
                const auto actual = _IsSlash(path[i]) ? L'\\' : _Fold(path[i]);
                const auto normalizedExpected = _IsSlash(expected) ? L'\\' : _Fold(expected);
                if (actual != normalizedExpected)
                {
                    return false;
                }
            }
            return true;
        }

        bool _IsOfficialWindowsPowerShellRoot(const Root& root) noexcept
        {
            const auto windowsDirectory = _GetWindowsDirectory();
            return windowsDirectory && root.absolute && _EqualsCi(root.leaf, L"powershell.exe") &&
                   (_EqualsWindowsDirectoryPath(root.token, *windowsDirectory, L"\\System32\\WindowsPowerShell\\v1.0\\powershell.exe") ||
                    _EqualsWindowsDirectoryPath(root.token, *windowsDirectory, L"\\SysWOW64\\WindowsPowerShell\\v1.0\\powershell.exe"));
        }

        bool _IsOfficialSystem32Root(const Root& root, const std::wstring_view leaf) noexcept
        {
            const auto windowsDirectory = _GetWindowsDirectory();
            if (!windowsDirectory || !root.absolute || !_EqualsCi(root.leaf, leaf))
            {
                return false;
            }

            if (_EqualsCi(leaf, L"wsl.exe"))
            {
                return _EqualsWindowsDirectoryPath(root.token, *windowsDirectory, L"\\System32\\wsl.exe");
            }
            return _EqualsWindowsDirectoryPath(root.token, *windowsDirectory, L"\\System32\\bash.exe");
        }

        bool _IsSysnativeAliasOfSystem32(const Root& commandRoot, const Root& actualRoot) noexcept
        {
            if (!_EqualsCi(commandRoot.leaf, actualRoot.leaf) ||
                (!_EqualsCi(commandRoot.leaf, L"wsl.exe") && !_EqualsCi(commandRoot.leaf, L"bash.exe")))
            {
                return false;
            }

            const auto windowsDirectory = _GetWindowsDirectory();
            if (!windowsDirectory)
            {
                return false;
            }
            if (_EqualsCi(commandRoot.leaf, L"wsl.exe"))
            {
                return _EqualsWindowsDirectoryPath(commandRoot.token, *windowsDirectory, L"\\Sysnative\\wsl.exe") &&
                       _IsOfficialSystem32Root(actualRoot, L"wsl.exe");
            }
            return _EqualsWindowsDirectoryPath(commandRoot.token, *windowsDirectory, L"\\Sysnative\\bash.exe") &&
                   _IsOfficialSystem32Root(actualRoot, L"bash.exe");
        }

        std::optional<std::vector<std::wstring>> _Tokenize(const std::wstring_view commandline) noexcept
        {
            if (commandline.empty())
            {
                return std::vector<std::wstring>{};
            }

            try
            {
                // A string_view may designate a non-NUL-terminated substring.
                // CommandLineToArgvW requires a NUL-terminated command line.
                const std::wstring nullTerminatedCommandline{ commandline };
                int argc = 0;
                const wil::unique_hlocal_ptr<LPWSTR> argv{ CommandLineToArgvW(nullTerminatedCommandline.c_str(), &argc) };
                if (!argv || argc <= 0)
                {
                    return std::nullopt;
                }

                std::vector<std::wstring> tokens;
                tokens.reserve(static_cast<size_t>(argc));
                for (int i = 0; i < argc; ++i)
                {
                    tokens.emplace_back(argv.get()[i]);
                }
                return tokens;
            }
            catch (...)
            {
                return std::nullopt;
            }
        }

        bool _OptionName(const std::wstring_view argument, const std::wstring_view name, const size_t minimum) noexcept
        {
            if (argument.size() < 2 || argument[0] != L'-')
            {
                return false;
            }
            const auto option = argument.substr(1);
            return option.size() >= minimum && option.size() <= name.size() && _StartsWithCi(name, option);
        }

        // Host switches with arguments that do not change profile loading or
        // interactivity. Their values must still be consumed so that a value
        // beginning with '-' is not accidentally interpreted as another host
        // switch.
        bool _IsPowerShellValueOption(const std::wstring_view argument) noexcept
        {
            return _OptionName(argument, L"ExecutionPolicy", 2) ||
                   _OptionName(argument, L"InputFormat", 2) ||
                   _OptionName(argument, L"OutputFormat", 2) ||
                   _OptionName(argument, L"Version", 1) ||
                   _OptionName(argument, L"WindowStyle", 2) ||
                   _OptionName(argument, L"WorkingDirectory", 2);
        }

        Classification _ClassifyPowerShell(const std::vector<std::wstring>& args, const bool core)
        {
            Classification result;
            result.shellKind = core ? ShellKind::PowerShell : ShellKind::WindowsPowerShell;
            bool noExit = false;

            for (size_t i = 1; i < args.size(); ++i)
            {
                const std::wstring_view arg{ args[i] };
                if (arg.empty() || arg[0] != L'-')
                {
                    result.reason = Reason::PowerShellCommand;
                    return result;
                }
                if (_OptionName(arg, L"NoProfile", 3))
                {
                    result.reason = Reason::PowerShellNoProfile;
                    return result;
                }
                if (_OptionName(arg, L"NonInteractive", 3))
                {
                    result.reason = Reason::PowerShellNonInteractive;
                    return result;
                }
                if (_OptionName(arg, L"File", 1))
                {
                    result.reason = Reason::PowerShellFile;
                    return result;
                }
                if (_OptionName(arg, L"Command", 1))
                {
                    result.eligible = noExit;
                    result.reason = noExit ? Reason::Eligible : Reason::PowerShellCommand;
                    return result; // Everything after this is command text.
                }
                if (_OptionName(arg, L"EncodedCommand", 2) || _EqualsCi(arg, L"-e"))
                {
                    result.reason = Reason::PowerShellEncodedCommand;
                    return result;
                }
                if (_OptionName(arg, L"NoExit", 3))
                {
                    noExit = true;
                    continue;
                }
                if (_OptionName(arg, L"NoLogo", 3) || _OptionName(arg, L"Mta", 1) || _OptionName(arg, L"Sta", 1))
                {
                    continue;
                }
                // pwsh -Login/-l changes login-shell setup but does not
                // suppress PowerShell profiles. Windows PowerShell does not
                // expose this host option, so keep its classification closed.
                if (core && _OptionName(arg, L"Login", 1))
                {
                    continue;
                }
                if (_IsPowerShellValueOption(arg))
                {
                    if (++i == args.size())
                    {
                        result.reason = Reason::PowerShellUnsupportedOption;
                        return result;
                    }
                    if (!args[i].empty() && args[i][0] == L'-')
                    {
                        result.reason = Reason::PowerShellUnsupportedOption;
                        return result;
                    }
                    continue;
                }

                result.reason = Reason::PowerShellUnsupportedOption;
                return result;
            }

            result.eligible = true;
            result.reason = Reason::Eligible;
            return result;
        }

        BashParseResult _ParseBash(
            const std::vector<std::wstring>& args,
            const size_t firstArgument,
            const bool allowLoginProfile)
        {
            bool login = false;
            bool noProfile = false;
            bool endOfOptions = false;

            for (size_t i = firstArgument; i < args.size(); ++i)
            {
                const std::wstring_view arg{ args[i] };
                if (endOfOptions)
                {
                    return BashParseResult::ScriptOperand;
                }
                if (arg == L"--")
                {
                    endOfOptions = true;
                    continue;
                }
                if (_EqualsCi(arg, L"--norc"))
                {
                    return BashParseResult::NoRc;
                }
                if (_EqualsCi(arg, L"--rcfile") || _EqualsCi(arg, L"--init-file") ||
                    _StartsWithCi(arg, L"--rcfile=") || _StartsWithCi(arg, L"--init-file="))
                {
                    return BashParseResult::CustomRcFile;
                }
                if (_EqualsCi(arg, L"--login"))
                {
                    login = true;
                    continue;
                }
                if (_EqualsCi(arg, L"--noprofile"))
                {
                    noProfile = true;
                    continue;
                }
                if (_EqualsCi(arg, L"--posix"))
                {
                    return BashParseResult::PosixMode;
                }
                if (_EqualsCi(arg, L"--noediting") || _EqualsCi(arg, L"--restricted") ||
                    _EqualsCi(arg, L"--debugger") || _EqualsCi(arg, L"--verbose") ||
                    _EqualsCi(arg, L"--dump-strings") || _EqualsCi(arg, L"--dump-po-strings"))
                {
                    continue;
                }
                if (_EqualsCi(arg, L"--help") || _EqualsCi(arg, L"--version") || _EqualsCi(arg, L"--pretty-print"))
                {
                    return BashParseResult::Command;
                }
                if (arg.size() < 2 || arg[0] != L'-' || arg == L"-")
                {
                    return BashParseResult::ScriptOperand;
                }

                for (size_t character = 1; character < arg.size(); ++character)
                {
                    switch (_Fold(arg[character]))
                    {
                    case L'i':
                        break;
                    case L'l':
                        login = true;
                        break;
                    case L'c':
                        return BashParseResult::Command;
                    case L'o':
                        // -o and -O consume their option name. Accepting a
                        // trailing cluster would require reproducing bash's
                        // option grammar, so reject it rather than guess.
                        if (character + 1 != arg.size() || ++i == args.size())
                        {
                            return BashParseResult::UnsupportedOption;
                        }
                        if (_EqualsCi(args[i], L"posix"))
                        {
                            return BashParseResult::PosixMode;
                        }
                        break;
                    case L's':
                        return BashParseResult::ScriptOperand;
                    case L't':
                        return BashParseResult::Command;
                    case L'a': case L'b': case L'e': case L'f': case L'h':
                    case L'k': case L'm': case L'p': case L'r':
                    case L'u': case L'v': case L'x':
                    case L'd':
                        break;
                    default:
                        return BashParseResult::UnsupportedOption;
                    }
                }
                continue;
            }

            if (login)
            {
                if (noProfile)
                {
                    return BashParseResult::LoginNoProfile;
                }
                return allowLoginProfile ? BashParseResult::Eligible : BashParseResult::LoginProfileUnproven;
            }
            // In the direct ConPTY-host context required by Classify, bash
            // with no command or script detects its TTY and is interactive.
            return BashParseResult::Eligible;
        }

        Reason _BashReason(const BashParseResult value) noexcept
        {
            switch (value)
            {
            case BashParseResult::Eligible: return Reason::Eligible;
            case BashParseResult::NoRc: return Reason::BashNoRc;
            case BashParseResult::CustomRcFile: return Reason::BashCustomRcFile;
            case BashParseResult::Command: return Reason::BashCommand;
            case BashParseResult::ScriptOperand: return Reason::BashScriptOperand;
            case BashParseResult::LoginNoProfile: return Reason::BashLoginNoProfile;
            case BashParseResult::LoginProfileUnproven: return Reason::BashLoginProfileUnproven;
            case BashParseResult::PosixMode: return Reason::BashPosixMode;
            default: return Reason::BashUnsupportedOption;
            }
        }

        void _SetBashOutcome(Classification& result, const BashParseResult parsed, const ShellKind kind)
        {
            result.shellKind = kind;
            result.eligible = parsed == BashParseResult::Eligible;
            result.reason = _BashReason(parsed);
        }

        bool _SetWslSelection(WslInvocation& wsl, const bool id, const std::wstring_view value) noexcept
        {
            if (value.empty() || value.front() == L'-' ||
                (id ? wsl.distributionId.has_value() : wsl.distribution.has_value()))
            {
                return false;
            }
            try
            {
                if (id)
                {
                    wsl.distributionId.emplace(value);
                }
                else
                {
                    wsl.distribution.emplace(value);
                }
                return true;
            }
            catch (...)
            {
                return false;
            }
        }

        Classification _ClassifyWsl(const std::vector<std::wstring>& args, const WslLauncher launcher)
        {
            Classification result;
            result.wsl.launcher = launcher;

            if (launcher == WslLauncher::LegacySystem32Bash)
            {
                result.wsl.guestShell = WslGuestShell::Bash;
                result.wsl.requiresIdentity = true; // Default distro is selected implicitly.
                _SetBashOutcome(result, _ParseBash(args, 1, false), ShellKind::WslBash);
                if (result.eligible)
                {
                    result.eligible = false;
                    result.reason = Reason::WslIdentityRequired;
                }
                return result;
            }

            size_t guestStart = args.size();
            for (size_t i = 1; i < args.size(); ++i)
            {
                const std::wstring_view arg{ args[i] };
                if (arg == L"--")
                {
                    guestStart = i + 1;
                    break;
                }
                if (_EqualsCi(arg, L"-e") || _EqualsCi(arg, L"--exec"))
                {
                    result.wsl.usesExec = true;
                    guestStart = i + 1;
                    break;
                }

                const auto takeValue = [&](const bool id) {
                    if (++i == args.size() || !_SetWslSelection(result.wsl, id, args[i]))
                    {
                        result.reason = Reason::WslMalformedHostOption;
                        return false;
                    }
                    return true;
                };
                if (_EqualsCi(arg, L"-d") || _EqualsCi(arg, L"--distribution"))
                {
                    if (!takeValue(false)) return result;
                    continue;
                }
                if (_StartsWithCi(arg, L"--distribution="))
                {
                    if (!_SetWslSelection(result.wsl, false, arg.substr(std::wstring_view{ L"--distribution=" }.size())))
                    {
                        result.reason = Reason::WslMalformedHostOption;
                        return result;
                    }
                    continue;
                }
                if (_EqualsCi(arg, L"--distribution-id"))
                {
                    if (!takeValue(true)) return result;
                    continue;
                }
                if (_StartsWithCi(arg, L"--distribution-id="))
                {
                    if (!_SetWslSelection(result.wsl, true, arg.substr(std::wstring_view{ L"--distribution-id=" }.size())))
                    {
                        result.reason = Reason::WslMalformedHostOption;
                        return result;
                    }
                    continue;
                }
                if (_EqualsCi(arg, L"-u") || _EqualsCi(arg, L"--user") || _EqualsCi(arg, L"--cd"))
                {
                    if (++i == args.size() || (!args[i].empty() && args[i][0] == L'-'))
                    {
                        result.reason = Reason::WslMalformedHostOption;
                        return result;
                    }
                    continue;
                }
                if (_EqualsCi(arg, L"--system") || _EqualsCi(arg, L"--debug-shell"))
                {
                    continue;
                }
                if (!arg.empty() && arg[0] == L'-')
                {
                    result.reason = Reason::WslUnsupportedHostOption;
                    return result;
                }
                guestStart = i;
                break;
            }

            if (guestStart == args.size())
            {
                if (result.wsl.usesExec)
                {
                    result.reason = Reason::WslMissingGuestCommand;
                    return result;
                }
                result.wsl.guestShell = WslGuestShell::Default;
                result.wsl.requiresIdentity = !result.wsl.distribution.has_value();
                result.wsl.requiresDefaultShellConfirmation = true;
                result.reason = result.wsl.requiresIdentity ? Reason::WslIdentityRequired : Reason::WslDefaultShellConfirmationRequired;
                return result;
            }

            const auto guestRoot = _RootOf(args[guestStart]);
            if (_EqualsCi(guestRoot.leaf, L"fish"))
            {
                result.wsl.guestShell = WslGuestShell::Fish;
                result.reason = Reason::WslExplicitFish;
                return result;
            }
            if (_EqualsCi(guestRoot.leaf, L"zsh"))
            {
                result.wsl.guestShell = WslGuestShell::Zsh;
                result.reason = Reason::WslExplicitZsh;
                return result;
            }
            if (!_EqualsCi(guestRoot.leaf, L"bash"))
            {
                result.wsl.guestShell = WslGuestShell::Other;
                result.reason = Reason::WslUnsupportedGuest;
                return result;
            }

            result.wsl.guestShell = WslGuestShell::Bash;
            _SetBashOutcome(result, _ParseBash(args, guestStart + 1, false), ShellKind::WslBash);
            if (result.eligible && !result.wsl.distribution.has_value())
            {
                // A distribution ID is deliberately not treated as a stable
                // identity. The caller needs to resolve it before creating a
                // profile-health TargetKey.
                result.wsl.requiresIdentity = true;
                result.eligible = false;
                result.reason = Reason::WslIdentityRequired;
            }
            return result;
        }
    }

    Classification Classify(const std::wstring_view effectiveCommandline,
                            const std::optional<std::wstring_view> actualRootImagePath) noexcept
    {
        Classification result;
        if (effectiveCommandline.empty())
        {
            return result;
        }

        const auto arguments = _Tokenize(effectiveCommandline);
        if (!arguments)
        {
            result.reason = Reason::CommandlineParseFailed;
            return result;
        }
        if (arguments->empty() || arguments->front().empty())
        {
            return result;
        }

        const auto commandRoot = _RootOf(arguments->front());
        std::optional<Root> actualRoot;
        if (actualRootImagePath)
        {
            actualRoot = _RootOf(*actualRootImagePath);
            if (actualRoot->leaf.empty() || !actualRoot->absolute)
            {
                result.reason = Reason::InvalidActualRoot;
                return result;
            }
            if (!_EqualsCi(commandRoot.leaf, actualRoot->leaf))
            {
                result.reason = Reason::ActualRootLeafMismatch;
                return result;
            }
            if (commandRoot.absolute && !_EqualsPathCiNormalized(commandRoot.token, actualRoot->token) &&
                !_IsSysnativeAliasOfSystem32(commandRoot, *actualRoot))
            {
                result.reason = Reason::ActualRootPathMismatch;
                return result;
            }
        }
        if (!commandRoot.absolute && (!actualRoot || !_IsBareLeaf(commandRoot.token)))
        {
            result.reason = Reason::RootNotAbsolute;
            return result;
        }

        const auto& provenRoot = actualRoot ? *actualRoot : commandRoot;
        if (_EqualsCi(commandRoot.leaf, L"powershell.exe") || _EqualsCi(commandRoot.leaf, L"pwsh.exe"))
        {
            if (_EqualsCi(commandRoot.leaf, L"powershell.exe") && !_IsOfficialWindowsPowerShellRoot(provenRoot))
            {
                result.reason = Reason::UnsupportedRoot;
                return result;
            }
            return _ClassifyPowerShell(*arguments, _EqualsCi(commandRoot.leaf, L"pwsh.exe"));
        }
        if (_EqualsCi(commandRoot.leaf, L"wsl.exe"))
        {
            if (!_IsOfficialSystem32Root(provenRoot, L"wsl.exe"))
            {
                result.reason = Reason::UnsupportedRoot;
                return result;
            }
            return _ClassifyWsl(*arguments, WslLauncher::WslExe);
        }
        if (_EqualsCi(commandRoot.leaf, L"bash.exe"))
        {
            if (_IsOfficialSystem32Root(provenRoot, L"bash.exe"))
            {
                return _ClassifyWsl(*arguments, WslLauncher::LegacySystem32Bash);
            }
            // This intentionally mirrors ShellIntegrationProfileGate: every
            // direct non-System32 bash.exe is Git-Bash-compatible. Its HOME
            // mapping remains outside this command-line-only classifier.
            _SetBashOutcome(result, _ParseBash(*arguments, 1, true), ShellKind::GitBash);
            return result;
        }

        result.reason = Reason::UnsupportedRoot;
        return result;
    }
}
