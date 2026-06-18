// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// WslShellIntegration.h
//
// WSL flavor — per-distro bash shell integration. Exposes WslBashFlavor,
// a concrete IShellFlavor that derives from BashFlavor. Construction
// resolves the distro's $HOME (cached per process) and builds the
// \\wsl$\<distro>\…\.bashrc + \\wsl$\<distro>\…\.intelligent-terminal\
// UNC paths; everything downstream is identical to native bash —
// ordinary fstream works transparently over the WSL UNC mount.
//
// The only WSL-specific work is:
//   1. Validate the distro name (strict allow-list — defends the
//      CreateProcessW command line against injection).
//   2. Probe $HOME inside the distro via one bounded wsl.exe spawn,
//      cached per-process so reconcile cycles after the first hit
//      are free.
//   3. Build the UNC paths.
//
// \\wsl$\ is Win10 1903+ (Build 18362); IT's WindowsTargetPlatformMinVersion
// is 10.0.18362.0, so this works on every supported host. The first
// access to \\wsl$\<dist>\ auto-starts the distro VM — so the $HOME
// probe pays the one-time cold-start cost.

#pragma once

#include "ShellIntegrationCommon.h"
#include "BashShellIntegration.h"
#include "ShellIntegrationProfileGate.h"

namespace Microsoft::Terminal::ShellIntegration::Wsl
{
    namespace details
    {
        // True for distro names that are safe to embed verbatim in a
        // `wsl.exe -d <name>` command line. WSL distro names allow
        // alphanumerics, `.`, `-`, `_`, and `+`; any other character
        // (especially `"`, `\`, `;`, `&`, `|`, newline, control bytes)
        // is rejected so we can never accidentally inject arbitrary
        // shell into the parent CreateProcessW command line.
        inline bool IsSafeDistroName(std::wstring_view name) noexcept
        {
            if (name.empty() || name.size() > 256)
            {
                return false;
            }
            for (const auto c : name)
            {
                const bool ok =
                    (c >= L'A' && c <= L'Z') ||
                    (c >= L'a' && c <= L'z') ||
                    (c >= L'0' && c <= L'9') ||
                    c == L'.' || c == L'-' || c == L'_' || c == L'+';
                if (!ok)
                {
                    return false;
                }
            }
            return true;
        }

        // True for posix-looking absolute paths we'd accept as a $HOME
        // reply from inside the distro. Rules:
        //   • must start with `/`
        //   • only ASCII letters, digits, `_`, `-`, `.`, `/`
        //   • no `//`, no trailing `/`
        //   • no segment composed entirely of `.` (rejects `..`, `.`,
        //     `...`, etc — fail-shut on traversal AND current-dir noise)
        inline bool IsSafeHome(std::string_view home) noexcept
        {
            if (home.size() < 2 || home.size() > 4096 || home.front() != '/' || home.back() == '/')
            {
                return false;
            }
            char prev = 0;
            for (const auto c : home)
            {
                const bool ok =
                    (c >= 'A' && c <= 'Z') ||
                    (c >= 'a' && c <= 'z') ||
                    (c >= '0' && c <= '9') ||
                    c == '_' || c == '-' || c == '.' || c == '/';
                if (!ok)
                {
                    return false;
                }
                if (c == '/' && prev == '/')
                {
                    return false;
                }
                prev = c;
            }
            // Reject any segment composed entirely of `.`.
            size_t segStart = 0;
            for (size_t i = 0; i <= home.size(); ++i)
            {
                if (i == home.size() || home[i] == '/')
                {
                    const auto seg = home.substr(segStart, i - segStart);
                    if (!seg.empty() && seg.find_first_not_of('.') == std::string_view::npos)
                    {
                        return false;
                    }
                    segStart = i + 1;
                }
            }
            return true;
        }

        // Result of probing a WSL profile for its identity.
        struct WslIdentity
        {
            std::wstring name; // $WSL_DISTRO_NAME reported by the distro
            std::string home;  // $HOME reported by the distro (POSIX path)
            bool valid() const noexcept { return !name.empty() && !home.empty(); }
        };

        // Return the portion of a profile commandline we should append our own
        // identity probe to. We must drop any command/operands the profile
        // already specifies, or the appended probe collides and fails.
        //
        //   * wsl.exe: KEEP the launcher + its distro-selection options
        //     (`-d` / `--distribution` / `--distribution-id` / `-u` / …) and
        //     cut at the command terminator (`-e` / `--exec` / `--`). Probe is
        //     then appended as `-e sh -c "…"`.
        //   * bash.exe: KEEP ONLY the launcher token and drop ALL its args
        //     (`~`, `-l`, `-c "…"`, …). bash treats the first non-option
        //     operand (e.g. `~`) as the script and ignores a later `-c`, so we
        //     replace the whole arg tail with our own `-c "…"`. bash.exe is the
        //     System32 WSL default-distro launcher and has no distro-selection
        //     options to preserve.
        //
        // Token-aware (whitespace-delimited, quote-respecting) so it tolerates
        // tabs / multiple spaces and a `--` at end-of-string, and only matches
        // a WHOLE token (never inside `--distribution-id` or a distro name).
        inline std::wstring_view StripExecTail(std::wstring_view cmd, bool isBash) noexcept
        {
            const auto isWs = [](wchar_t c) noexcept { return c == L' ' || c == L'\t'; };
            // Advance `i` past one token starting at the current position
            // (quote-aware); returns [tokStart, i).
            size_t i = 0;
            const auto nextToken = [&]() noexcept -> std::wstring_view {
                while (i < cmd.size() && isWs(cmd[i]))
                {
                    ++i;
                }
                const size_t tokStart = i;
                if (i < cmd.size() && cmd[i] == L'"')
                {
                    ++i;
                    while (i < cmd.size() && cmd[i] != L'"')
                    {
                        ++i;
                    }
                    if (i < cmd.size())
                    {
                        ++i; // consume closing quote
                    }
                }
                else
                {
                    while (i < cmd.size() && !isWs(cmd[i]))
                    {
                        ++i;
                    }
                }
                return cmd.substr(tokStart, i - tokStart);
            };

            // bash.exe: keep only the launcher token.
            (void)nextToken();
            if (isBash)
            {
                return cmd.substr(0, i);
            }

            // wsl.exe: keep distro-selection options; cut at the first command
            // terminator token.
            while (i < cmd.size())
            {
                const size_t tokStart = i;
                const std::wstring_view tok = nextToken();
                if (tok == L"-e" || tok == L"--exec" || tok == L"--")
                {
                    size_t cut = tokStart;
                    while (cut > 0 && isWs(cmd[cut - 1]))
                    {
                        --cut;
                    }
                    return cmd.substr(0, cut);
                }
            }
            return cmd;
        }

        // If the launch token is a BARE `wsl(.exe)` / `bash(.exe)` (no path,
        // unquoted), qualify it to the OS copy under `<WindowsDir>\System32`.
        // Our identity probe runs automatically (settings reconcile / FRE), so
        // a bare token passed to `CreateProcessW(nullptr, …)` — whose search
        // order includes the CURRENT DIRECTORY — could otherwise auto-run a
        // planted same-named binary. Mirrors WslDistroGenerator's GH#11096
        // hardening. A token that already carries a path (or is quoted, i.e.
        // user-qualified) is returned unchanged.
        inline std::wstring QualifyBareLauncher(std::wstring_view selection)
        {
            namespace SI = ::Microsoft::Terminal::ShellIntegration;
            size_t start = 0;
            while (start < selection.size() && (selection[start] == L' ' || selection[start] == L'\t'))
            {
                ++start;
            }
            // A quoted launcher is taken as-is (it's an explicit path).
            if (start >= selection.size() || selection[start] == L'"')
            {
                return std::wstring{ selection };
            }
            size_t end = start;
            while (end < selection.size() && selection[end] != L' ' && selection[end] != L'\t')
            {
                ++end;
            }
            const std::wstring_view tok = selection.substr(start, end - start);
            for (const auto c : tok)
            {
                if (c == L'\\' || c == L'/' || c == L':')
                {
                    return std::wstring{ selection }; // already has a path
                }
            }
            std::wstring leaf;
            if (SI::details::EqualsCi(tok, L"wsl") || SI::details::EqualsCi(tok, L"wsl.exe"))
            {
                leaf = L"wsl.exe";
            }
            else if (SI::details::EqualsCi(tok, L"bash") || SI::details::EqualsCi(tok, L"bash.exe"))
            {
                leaf = L"bash.exe";
            }
            else
            {
                return std::wstring{ selection };
            }
            std::wstring out = SI::details::WindowsDir();
            out += L"\\System32\\";
            out += leaf;
            out.append(selection.substr(end)); // preserve the option tail
            return out;
        }

        // Run the profile's launch commandline (distro SELECTION only — see
        // StripExecTail) with a probe appended, and read back the distro's own
        // `$WSL_DISTRO_NAME` and `$HOME`. We NEVER parse the distro out of the
        // commandline — the profile already selects it (`-d <name>`,
        // `--distribution-id {GUID}`, or the default distro for bare `wsl.exe`
        // / System32 `bash.exe`), so we reuse the command and let the running
        // distro identify itself. This makes Source / `--distribution-id` /
        // renamed profiles all "just work" with one code path.
        //
        // The shell-invocation suffix differs by launcher:
        //   * `wsl.exe …` -> ` -e sh -c "echo $WSL_DISTRO_NAME; echo $HOME"`
        //     (wsl.exe is a launcher, not a shell — it must be given a shell
        //     to run; bare `-c` is rejected by wsl.exe).
        //   * `bash.exe`  -> ` -c "echo $WSL_DISTRO_NAME; echo $HOME"`
        //     (bash.exe IS the shell; `-c` is its own flag, `-e`/`-d` are
        //     rejected).
        //
        // Returns an invalid (empty) WslIdentity on any failure (no WSL,
        // distro stopped + can't auto-start, timeout, garbled / unsafe
        // output). `WSL_UTF8=1` forces wsl.exe to relay stdout as UTF-8.
        // Bounded at 30s wall-clock to absorb WSL2 cold-start.
        inline WslIdentity QueryWslIdentityRaw(std::wstring_view launchCommandline) noexcept
        {
            if (launchCommandline.empty())
            {
                return {};
            }
            try
            {
                SECURITY_ATTRIBUTES sa{};
                sa.nLength = sizeof(sa);
                sa.bInheritHandle = TRUE;

                HANDLE rawRead = nullptr;
                HANDLE rawWrite = nullptr;
                if (!CreatePipe(&rawRead, &rawWrite, &sa, 0))
                {
                    return {};
                }
                wil::unique_handle readEnd{ rawRead };
                wil::unique_handle writeEnd{ rawWrite };
                SetHandleInformation(readEnd.get(), HANDLE_FLAG_INHERIT, 0);

                STARTUPINFOW si{};
                si.cb = sizeof(si);
                si.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
                si.wShowWindow = SW_HIDE;
                si.hStdOutput = writeEnd.get();
                si.hStdError = writeEnd.get();
                si.hStdInput = GetStdHandle(STD_INPUT_HANDLE);

                // Build the probe commandline: the profile's distro SELECTION
                // (launcher + `-d`/`--distribution-id`/… options, with any
                // command the profile already specifies stripped — see
                // StripExecTail) plus OUR identity probe. Stripping first means
                // a profile like `wsl.exe -d Ubuntu -e fish` or `bash.exe -c
                // "..."` doesn't get a colliding second `-e`/`-c` that would
                // corrupt parsing and fail the probe. The flag depends on
                // whether the launcher is itself a shell (bash.exe -> `-c`) or
                // a launcher (wsl.exe -> `-e sh -c`).
                //
                // No IsSafeDistroName guard on the INPUT here: we don't build a
                // distro selector from user-editable text (the old injection
                // vector) — we reuse the profile's own options (which the user
                // already runs) and only validate the distro identity the probe
                // REPORTS, before using it to build the \\wsl$ path below.
                const bool isBash =
                    ::Microsoft::Terminal::ShellIntegration::details::CommandlineHasExeToken(launchCommandline, L"bash");
                std::wstring cmdLine{ QualifyBareLauncher(StripExecTail(launchCommandline, isBash)) };
                if (isBash)
                {
                    cmdLine += L" -c \"echo $WSL_DISTRO_NAME; echo $HOME\"";
                }
                else
                {
                    cmdLine += L" -e sh -c \"echo $WSL_DISTRO_NAME; echo $HOME\"";
                }

                // Expand %VAR% in the launcher path. CreateProcessW does NOT
                // expand environment variables, but Windows Terminal expands
                // them when it launches a profile, so a profile commandline
                // such as `%SystemRoot%\System32\wsl.exe -d Ubuntu` must
                // resolve here too (matching how it actually runs). The probe
                // suffix uses shell `$VAR` syntax, which Expand… leaves alone.
                if (const DWORD needed = ExpandEnvironmentStringsW(cmdLine.c_str(), nullptr, 0); needed > 1)
                {
                    std::wstring expanded(needed, L'\0');
                    const DWORD wrote = ExpandEnvironmentStringsW(cmdLine.c_str(), expanded.data(), needed);
                    if (wrote != 0 && wrote <= needed)
                    {
                        expanded.resize(wrote - 1); // drop trailing NUL
                        cmdLine = std::move(expanded);
                    }
                }

                // Pass WSL_UTF8=1 via a child-only environment block
                // instead of mutating the process-wide environment.
                // SetEnvironmentVariableW would let other threads in
                // this process inherit WSL_UTF8=1 in any CreateProcess*
                // they spawn during the 30-second window below — even
                // a narrowed Set/restore window cannot eliminate that
                // race. The child env block we hand to CreateProcessW
                // via lpEnvironment is private to the wsl.exe spawn:
                // no other thread sees WSL_UTF8, and we don't need
                // any save/restore dance.
                //
                // WSL_UTF8=1 makes wsl.exe relay child stdout as UTF-8
                // (otherwise the default is UTF-16LE for newer WSL).
                //
                // Fallback: if GetEnvironmentStringsW() fails (very rare
                // — only under low-memory pressure) we cannot safely
                // build a child env block (passing one with only
                // WSL_UTF8=1 would drop SystemRoot/Path and break the
                // wsl.exe spawn). In that case, pass nullptr to inherit
                // the parent env without the WSL_UTF8 override. Newer
                // WSL may then emit UTF-16LE stdout; the IsSafeHome
                // validator below rejects that and we return empty
                // (same as any other probe failure), which the cache
                // policy treats as retryable.
                bool useChildEnv = false;
                std::wstring childEnv;
                if (wchar_t* origEnvBlock = GetEnvironmentStringsW())
                {
                    bool wslUtf8Replaced = false;
                    for (wchar_t* p = origEnvBlock; *p != L'\0'; )
                    {
                        const std::wstring_view entry{ p };
                        p += entry.size() + 1;
                        // Strip any existing WSL_UTF8=... so we never
                        // emit two definitions. Case-insensitive match
                        // on the leading name (Windows env names are
                        // case-insensitive).
                        if (entry.size() >= 9 &&
                            _wcsnicmp(entry.data(), L"WSL_UTF8=", 9) == 0)
                        {
                            // Emit our canonical WSL_UTF8=1 exactly
                            // ONCE, even if the parent env (rarely)
                            // contains multiple WSL_UTF8 entries.
                            // Subsequent duplicates are silently
                            // dropped — the env block is now de-duped
                            // and the child sees a single definition.
                            if (!wslUtf8Replaced)
                            {
                                childEnv.append(L"WSL_UTF8=1");
                                childEnv.push_back(L'\0');
                                wslUtf8Replaced = true;
                            }
                            // Skip pushing anything for duplicate
                            // entries (don't even emit a separator —
                            // env entries are NUL-terminated, and a
                            // skipped entry leaves no trace).
                        }
                        else
                        {
                            childEnv.append(entry);
                            childEnv.push_back(L'\0');
                        }
                    }
                    FreeEnvironmentStringsW(origEnvBlock);
                    if (!wslUtf8Replaced)
                    {
                        childEnv.append(L"WSL_UTF8=1");
                        childEnv.push_back(L'\0');
                    }
                    childEnv.push_back(L'\0'); // env block terminates on \0\0
                    useChildEnv = true;
                }

                PROCESS_INFORMATION pi{};
                const DWORD creationFlags = CREATE_NO_WINDOW |
                                            (useChildEnv ? CREATE_UNICODE_ENVIRONMENT : 0u);
                const bool spawnOk = CreateProcessW(nullptr,
                                                    cmdLine.data(),
                                                    nullptr,
                                                    nullptr,
                                                    TRUE,
                                                    creationFlags,
                                                    useChildEnv ? childEnv.data() : nullptr,
                                                    nullptr,
                                                    &si,
                                                    &pi) != FALSE;
                if (!spawnOk)
                {
                    return {};
                }
                wil::unique_handle process{ pi.hProcess };
                wil::unique_handle thread{ pi.hThread };

                writeEnd.reset();

                if (WaitForSingleObject(process.get(), 30000) != WAIT_OBJECT_0)
                {
                    TerminateProcess(process.get(), 1);
                    WaitForSingleObject(process.get(), 1000);
                    return {};
                }

                DWORD exitCode = 0;
                if (!GetExitCodeProcess(process.get(), &exitCode) || exitCode != 0)
                {
                    return {};
                }

                std::string raw;
                char buf[256];
                DWORD bytesRead = 0;
                while (raw.size() < 4096 &&
                       ReadFile(readEnd.get(), buf, sizeof(buf), &bytesRead, nullptr) &&
                       bytesRead > 0)
                {
                    raw.append(buf, bytesRead);
                }

                while (!raw.empty() &&
                       (raw.back() == '\n' || raw.back() == '\r' ||
                        raw.back() == ' ' || raw.back() == '\t'))
                {
                    raw.pop_back();
                }
                // The probe prints two lines: `$WSL_DISTRO_NAME` then `$HOME`.
                // Any WSL cold-start banner precedes them, so take the LAST
                // two non-empty lines: home is the last line, name the one
                // before it.
                std::string home = raw;
                std::string nameUtf8;
                if (const auto lastLf = raw.find_last_of('\n'); lastLf != std::string::npos)
                {
                    home = raw.substr(lastLf + 1);
                    std::string before = raw.substr(0, lastLf);
                    while (!before.empty() &&
                           (before.back() == '\n' || before.back() == '\r' ||
                            before.back() == ' ' || before.back() == '\t'))
                    {
                        before.pop_back();
                    }
                    const auto lf2 = before.find_last_of('\n');
                    nameUtf8 = (lf2 != std::string::npos) ? before.substr(lf2 + 1) : before;
                }

                if (!IsSafeHome(home))
                {
                    return {};
                }
                // Distro names are ASCII (IsSafeDistroName enforces it), so a
                // byte-wise widen is correct here.
                const std::wstring nameW{ nameUtf8.begin(), nameUtf8.end() };
                if (!IsSafeDistroName(nameW))
                {
                    return {};
                }
                WslIdentity id;
                id.name = nameW;
                id.home = std::move(home);
                return id;
            }
            catch (...)
            {
                return {};
            }
        }

        // Per-process cache of launch-commandline → WslIdentity, populated
        // lazily on first access. Each distinct profile command pays the
        // cold-start cost at most once per process per successful probe.
        // Only successful probes are cached: a failed probe (distro stopped,
        // transient WSL startup race, unreadable identity) is NOT memoized,
        // so a fresh Install/Uninstall call from the user (Settings UI / FRE
        // retry) re-probes from scratch.
        // Mutex protects only the cache MAP read/write — the probe itself
        // runs unlocked (double-checked locking below). Two concurrent
        // callers for the same command can both spawn; that's intentional
        // (serializing across the up-to-30s cold-start would block unrelated
        // callers). The second writer's insert is dropped on the re-check.
        inline WslIdentity GetWslIdentityCached(std::wstring_view launchCommandline, bool allowProbe = true)
        {
            static std::mutex cacheMu;
            static std::map<std::wstring, WslIdentity, std::less<>> cache;

            {
                std::lock_guard<std::mutex> g{ cacheMu };
                if (const auto it = cache.find(launchCommandline); it != cache.end())
                {
                    return it->second;
                }
            }
            // Cache-only peek (allowProbe=false): used to read back a distro
            // identity already resolved by a prior Install, without paying
            // (or risking) another cold-start probe.
            if (!allowProbe)
            {
                return {};
            }
            // Probe without the lock (see comment above).
            auto id = QueryWslIdentityRaw(launchCommandline);
            {
                std::lock_guard<std::mutex> g{ cacheMu };
                if (const auto it = cache.find(launchCommandline); it != cache.end())
                {
                    return it->second;
                }
                if (id.valid())
                {
                    cache.emplace(std::wstring{ launchCommandline }, id);
                }
            }
            return id;
        }
    }

    // Returns the distro name a prior Install/Uninstall already resolved for
    // this commandline, or empty if none is cached (never triggers a probe).
    // Used for human-readable labels (e.g. error dialogs) so we surface the
    // actual `$WSL_DISTRO_NAME` instead of the raw launch commandline.
    inline std::wstring ProbedDistroName(std::wstring_view launchCommandline)
    {
        return details::GetWslIdentityCached(launchCommandline, /*allowProbe*/ false).name;
    }

    // Build a Win32 UNC path for an arbitrary POSIX path inside a WSL
    // distro: \\wsl$\<distName>\<posixPath-with-forward-slashes-converted-to-backslashes>.
    // Both forward and backslash separators after the distro name are
    // accepted by the Win32 file APIs (the \\wsl$\ provider routes the
    // lookup through the distro's vfs either way), but we emit the
    // canonical backslash form so the produced path matches what other
    // Windows tools display and so it sails through any consumer that
    // treats them differently. A leading `/` in the posix path is
    // trimmed to avoid producing `\\wsl$\Ubuntu\\path` (double-sep).
    inline std::wstring UncPath(std::wstring_view distName, std::string_view posixPath)
    {
        std::wstring out{ L"\\\\wsl$\\" };
        out.append(distName);
        if (!posixPath.empty() && posixPath.front() == '/')
        {
            posixPath.remove_prefix(1);
        }
        out.push_back(L'\\');
        for (const auto c : posixPath)
        {
            out.push_back(c == '/' ? L'\\' : static_cast<wchar_t>(static_cast<unsigned char>(c)));
        }
        return out;
    }

    // Concrete IShellFlavor for a WSL bash profile. The constructor probes
    // the distro IDENTITY by running the profile's exact launch commandline
    // (`launchCommandline`) with an appended `$WSL_DISTRO_NAME` / `$HOME`
    // probe — it does NOT parse the distro out of the command. Callers check
    // Valid() before handing the instance to the orchestrator.
    //
    // Reuses BashFlavor for every IShellFlavor method — once the UNC paths
    // are resolved the install/uninstall flow IS native bash operating over
    // the \\wsl$\ mount.
    //
    // Construction can block up to 30s on first use of a cold distro (the
    // probe spins up the WSL2 VM). Subsequent constructions for the same
    // commandline hit the per-process cache.
    class WslBashFlavor : public Bash::BashFlavor
    {
    public:
        explicit WslBashFlavor(std::wstring launchCommandline) :
            // Initialize the BashFlavor base with empty paths first, then
            // patch them in the body once we've probed. Empty paths are
            // harmless: the orchestrator only runs after we check Valid().
            Bash::BashFlavor{ {}, {} }
        {
            if (launchCommandline.empty())
            {
                _errorMessage = L"WSL profile commandline is empty";
                return;
            }
            const auto id = details::GetWslIdentityCached(launchCommandline);
            if (!id.valid())
            {
                _errorMessage = L"Could not probe WSL distro identity ($WSL_DISTRO_NAME / $HOME)";
                return;
            }
            // id.name / id.home were already validated by the probe
            // (IsSafeDistroName / IsSafeHome) before being returned.
            _distName = id.name;
            _profilePath = UncPath(_distName, id.home + "/.bashrc");
            _scriptDir = std::filesystem::path{ UncPath(_distName, id.home + "/.intelligent-terminal") };
            _valid = true;
        }

        bool Valid() const noexcept { return _valid; }
        std::wstring_view ErrorMessage() const noexcept { return _errorMessage; }

        std::wstring          ProfilePath() const override          { return _profilePath; }
        std::filesystem::path ScriptDir() const override            { return _scriptDir; }
        // Everything else (script filename / content / block / orphan
        // recovery / line-ending policy) is inherited from BashFlavor.

    private:
        std::wstring _distName;
        std::wstring _profilePath;
        std::filesystem::path _scriptDir;
        std::wstring _errorMessage;
        bool _valid{ false };
    };

    // Install bash shell integration into the WSL distro a profile launches.
    // `launchCommandline` is the profile's exact commandline (e.g.
    // `C:\Windows\system32\wsl.exe --distribution-id {GUID}`,
    // `wsl.exe -d Ubuntu`, or `C:\Windows\System32\bash.exe`).
    //
    // Synchronous — call from a background thread. The first call for each
    // commandline can block up to 30s on a cold-start; subsequent calls
    // return immediately from the cache.
    inline InstallResult Install(const std::wstring& launchCommandline)
    {
        WslBashFlavor flavor{ launchCommandline };
        if (!flavor.Valid())
        {
            return { false, false, std::wstring{ flavor.ErrorMessage() } };
        }
        return orchestrator::Install(flavor);
    }

    inline InstallResult Uninstall(const std::wstring& launchCommandline)
    {
        WslBashFlavor flavor{ launchCommandline };
        if (!flavor.Valid())
        {
            // If we can't reach the distro there's nothing to remove —
            // treat as already-uninstalled so a toggle-off reconcile
            // doesn't flap into an error state every time the distro
            // is down.
            return { true, true, {} };
        }
        return orchestrator::Uninstall(flavor);
    }
}
