// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "TerminalPage.h"
#include "TerminalPaneContent.h"
#include "AgentPaneContent.h"
#include "BugReportDiagnostics.h"
#include "../inc/AgentPolicy.h"

#include <bcrypt.h>
#include <map>

#pragma comment(lib, "bcrypt.lib")

namespace Diagnostics = IntelligentTerminal::Diagnostics;
namespace AgentPolicy = Microsoft::Terminal::Settings::Model::AgentPolicy;

namespace winrt::TerminalApp::implementation
{
    Diagnostics::Report TerminalPage::_BugReportSnapshot()
    {
        Diagnostics::Report report;
        auto& value = report.value;
        value["schema_version"] = 1;
        value["collection_started_utc"] = Diagnostics::UtcNow();
        value["collection_errors"] = Json::Value{ Json::arrayValue };
        value["collection_error_count"] = Json::UInt64{ 0 };
        value["scope"] = "current_window_and_owned_agent_processes";
        value["request_history"] = "logs_only_snapshot_is_not_request_time_state";
        value["logs_privacy"] = "raw_logs_not_redacted";

        const auto pin = [&](HANDLE handle, Json::Value owner) {
            if (report.processes.size() >= 64)
            {
                Diagnostics::Error(value, "process_snapshot", "limit_exceeded");
                return;
            }
            wil::unique_handle copy;
            if (handle && DuplicateHandle(GetCurrentProcess(), handle, GetCurrentProcess(), copy.put(), 0, FALSE, DUPLICATE_SAME_ACCESS))
                report.processes.push_back({ std::move(owner), std::move(copy) });
            else
            {
                Diagnostics::Error(value, "process_snapshot", "handle_unavailable");
                report.processes.push_back({ std::move(owner), {} });
            }
        };
        Json::Value terminal;
        terminal["role"] = "terminal";
        pin(GetCurrentProcess(), std::move(terminal));

        try
        {
            Json::Value master;
            master["role"] = "wta_master";
            master["relationship"] = "owned_master_not_proof_of_helper_connection";
            auto handle = SharedWta::Instance().DiagnosticProcessHandle();
            if (handle)
            {
                value["owned_master"] = Diagnostics::ProcessIdentity(handle.get());
                pin(handle.get(), std::move(master));
            }
            else
                value["owned_master"]["status"] = "not_running";
        }
        catch (...)
        {
            Diagnostics::Error(value, "master_snapshot", "unavailable");
        }

        try
        {
            value["window_id"] = Json::UInt64{ _WindowProperties.WindowId() };
            const auto focused = _GetFocusedTabIndex();
            value["focused_tab_index"] = focused ? Json::Value{ *focused } : Json::Value{};
            value["tabs"] = Json::Value{ Json::arrayValue };
            const auto globals = _settings.GlobalSettings();
            auto& settings = value["effective_settings"];
            settings["acp_agent"] = Diagnostics::Provider(globals.AcpAgent());
            settings["delegate_agent"] = Diagnostics::Provider(globals.DelegateAgent());
            settings["agent_pane_position"] = Diagnostics::Enum(globals.AgentPanePosition(), { L"top", L"bottom", L"left", L"right" });
            settings["auto_fix_enabled"] = globals.AutoFixEnabled();
            settings["auto_error_detection_enabled"] = globals.AutoErrorDetectionEnabled();
            settings["confirmation_read"] = Diagnostics::Enum(globals.AiConfirmationReadOps(), { L"auto", L"ask" });
            settings["confirmation_create"] = Diagnostics::Enum(globals.AiConfirmationCreateOps(), { L"auto", L"ask" });
            settings["confirmation_input"] = Diagnostics::Enum(globals.AiConfirmationInputOps(), { L"auto", L"ask" });
            settings["allowed_agents_policy_configured"] = AgentPolicy::IsAllowedAgentsPolicyConfigured();
            settings["custom_agent_policy_allowed"] = AgentPolicy::IsCustomAgentAllowed();
            settings["auto_fix_policy_allowed"] = AgentPolicy::IsAutoFixAllowed();
            settings["yolo_policy_allowed"] = AgentPolicy::IsYoloModeAllowed();
            settings["coordinator_enabled"] = globals.AiCoordinatorEnabled();
            for (const std::wstring_view provider : { L"copilot", L"claude", L"codex", L"gemini", L"opencode" })
                settings["policy_allowed_providers"][Diagnostics::Ascii(provider)] = AgentPolicy::IsAgentAllowed(provider);
            for (const auto* name : { "acp_agent", "delegate_agent", "agent_pane_position", "confirmation_read", "confirmation_create", "confirmation_input" })
            {
                if (settings[name] == "unavailable")
                    Diagnostics::Error(value, "effective_settings", "unknown_enum_omitted");
            }

            uint32_t paneCount = 0;
            for (uint32_t index = 0; index < _tabs.Size(); ++index)
            {
                if (index >= 128)
                {
                    Diagnostics::Error(value, "tabs", "limit_exceeded");
                    break;
                }
                Json::Value tabValue;
                tabValue["tab_index"] = index;
                try
                {
                    const auto tab = _GetTabImpl(_tabs.GetAt(index));
                    if (!tab)
                    {
                        tabValue["status"] = "not_terminal_tab";
                        value["tabs"].append(std::move(tabValue));
                        continue;
                    }
                    tabValue["stable_tab_id"] = winrt::to_string(tab->StableId());
                    const auto active = tab->GetActivePane();
                    tabValue["active_pane_id"] = active && active->Id() ? Json::Value{ *active->Id() } : Json::Value{};
                    tabValue["agent_stashed"] = tab->HasStashedAgentPane();
                    tabValue["agent_prewarm_suppressed"] = tab->AgentPrewarmSuppressed();
                    tabValue["effective_agent"] = Diagnostics::Provider(tab->AgentIdOverride().empty() ? globals.AcpAgent() : tab->AgentIdOverride());
                    tabValue["effective_agent_pane_position"] = Diagnostics::Enum(tab->EffectiveAgentPanePosition(globals.AgentPanePosition()), { L"top", L"bottom", L"left", L"right" });
                    tabValue["panes"] = Json::Value{ Json::arrayValue };
                    if (const auto root = tab->GetRootPane())
                    {
                        root->WalkTree([&](const auto& pane) {
                            if (!pane->GetContent())
                                return;
                            if (++paneCount > 512)
                                return;
                            Json::Value item;
                            item["pane_id"] = pane->Id() ? Json::Value{ *pane->Id() } : Json::Value{};
                            item["content_id"] = pane->ContentId() ? Json::Value{ *pane->ContentId() } : Json::Value{};
                            item["session_id"] = winrt::to_string(winrt::to_hstring(pane->GetSessionId()));
                            item["kind"] = pane->IsAgentPane() ? "agent" : pane->GetTerminalControl() ? "terminal" : "other";
                            item["source_of_agent"] = pane->IsSourceOfAgentPane();
                            auto content = pane->GetContent().try_as<TerminalApp::TerminalPaneContent>();
                            if (const auto agent = pane->GetContent().try_as<TerminalApp::AgentPaneContent>())
                            {
                                content = agent.GetTerminalContent();
                                const auto impl = winrt::get_self<AgentPaneContent>(agent);
                                item["helper_event_ready"] = impl->IsHelperEventReady();
                                item["helper_origin_metadata_source"] = "prompt_context_logs";
                                const auto transfer = winrt::to_string(impl->TransferSourceTabId());
                                if (Diagnostics::IsGuid(transfer))
                                    item["transfer_source_stable_tab_id"] = transfer;
                            }
                            item["profile_settings_status"] = content ? "unavailable" : "not_applicable";
                            if (content)
                            {
                                const auto profile = winrt::get_self<TerminalPaneContent>(content)->GetProfile();
                                if (profile)
                                {
                                    item["profile_reload_environment_variables"] = profile.ReloadEnvironmentVariables();
                                    item["profile_settings_status"] = "available";
                                }
                                else
                                    Diagnostics::Error(value, "profile_settings", "unavailable");
                            }
                            if (pane->IsAgentPane())
                            {
                                Json::Value owner;
                                owner["role"] = "wta_helper";
                                owner["window_id"] = value["window_id"];
                                owner["stable_tab_id"] = tabValue["stable_tab_id"];
                                owner["pane_id"] = item["pane_id"];
                                owner["session_id"] = item["session_id"];
                                owner["connected_master_identity"] = "request_time_logs_only";
                                const auto control = pane->GetTerminalControl();
                                if (control)
                                {
                                    if (const auto connection = control.Connection().try_as<Microsoft::Terminal::TerminalConnection::ConptyConnection>())
                                    {
                                        wil::unique_handle process{ reinterpret_cast<HANDLE>(connection.DuplicateRootProcessHandle()) };
                                        pin(process.get(), std::move(owner));
                                    }
                                    else
                                        Diagnostics::Error(value, "helper_snapshot", "connection_unavailable");
                                }
                                else
                                    Diagnostics::Error(value, "helper_snapshot", "control_unavailable");
                            }
                            tabValue["panes"].append(std::move(item));
                        });
                    }
                    else
                    {
                        Diagnostics::Error(value, "tab_snapshot", "root_unavailable");
                        tabValue["status"] = "partial";
                    }
                    if (tabValue["status"].isNull())
                        tabValue["status"] = "collected";
                }
                catch (...)
                {
                    tabValue["status"] = "partial";
                    Diagnostics::Error(value, "tab_snapshot", "unavailable");
                }
                value["tabs"].append(std::move(tabValue));
            }
            if (paneCount > 512)
                Diagnostics::Error(value, "panes", "limit_exceeded");
        }
        catch (...)
        {
            Diagnostics::Error(value, "ui_snapshot", "unavailable");
        }
        value["ui_snapshot_completed_utc"] = Diagnostics::UtcNow();
        return report;
    }
}

namespace IntelligentTerminal::Diagnostics
{
    static const char* Architecture(const USHORT machine)
    {
        switch (machine)
        {
        case IMAGE_FILE_MACHINE_AMD64: return "x64";
        case IMAGE_FILE_MACHINE_ARM64: return "arm64";
        case IMAGE_FILE_MACHINE_I386: return "x86";
        default: return "unavailable";
        }
    }

    static void ReadImageMetadata(HANDLE file, Json::Value& binary, Json::Value& report, const bool wta)
    {
        const auto readAt = [&](const LONGLONG offset, void* data, const DWORD size) {
            LARGE_INTEGER position{};
            position.QuadPart = offset;
            DWORD read{};
            return SetFilePointerEx(file, position, nullptr, FILE_BEGIN) &&
                   ReadFile(file, data, size, &read, nullptr) && read == size;
        };
        IMAGE_DOS_HEADER dos{};
        DWORD signature{};
        IMAGE_FILE_HEADER header{};
        if (!readAt(0, &dos, sizeof(dos)) || dos.e_magic != IMAGE_DOS_SIGNATURE ||
            dos.e_lfanew < 0 || dos.e_lfanew > 1024 * 1024 ||
            !readAt(dos.e_lfanew, &signature, sizeof(signature)) || signature != IMAGE_NT_SIGNATURE ||
            !readAt(dos.e_lfanew + sizeof(signature), &header, sizeof(header)))
        {
            Error(report, "image_metadata", "invalid_pe_header");
            return;
        }
        binary["architecture"] = Architecture(header.Machine);
        if (!wta)
            return;
        binary["embedded_build_status"] = "unavailable";
        if (header.NumberOfSections > 96 || header.SizeOfOptionalHeader > 4096)
        {
            Error(report, "embedded_build", "section_limit");
            return;
        }
        const LONGLONG sectionStart = dos.e_lfanew + sizeof(signature) + sizeof(header) + header.SizeOfOptionalHeader;
        for (WORD i = 0; i < header.NumberOfSections; ++i)
        {
            IMAGE_SECTION_HEADER section{};
            if (!readAt(sectionStart + i * sizeof(section), &section, sizeof(section)))
                break;
            if (memcmp(section.Name, ".wtadiag", 8) != 0)
                continue;
            char record[128]{};
            if (section.SizeOfRawData < sizeof(record) || !readAt(section.PointerToRawData, record, sizeof(record)) ||
                memcmp(record, "WTA-DIAG-1\0", 11) != 0)
                break;
            const std::string cargo{ record + 16, strnlen_s(record + 16, 32) };
            const std::string commit{ record + 48, strnlen_s(record + 48, 65) };
            if (IsVersion(cargo))
            {
                binary["cargo_version"] = cargo;
                binary["embedded_build_status"] = "collected";
            }
            if ((commit.size() == 40 || commit.size() == 64) &&
                std::all_of(commit.begin(), commit.end(), [](const char ch) {
                    return (ch >= '0' && ch <= '9') || (ch >= 'a' && ch <= 'f') || (ch >= 'A' && ch <= 'F');
                }))
            {
                binary["build_commit"] = commit;
                binary["build_commit_status"] = "available";
            }
            binary["build_dirty_state"] = "not_recorded";
            if (binary["embedded_build_status"] != "collected")
                Error(report, "embedded_build", "invalid_record");
            return;
        }
        Error(report, "embedded_build", "record_unavailable");
    }

    static Json::Value HashProduct(const std::filesystem::path& path, const wchar_t* expectedName, Json::Value& report)
    {
        Json::Value binary;
        binary["status"] = "unavailable";
        binary["sha256_measurement"] = "file_bytes_at_collection_not_mapped_memory";
        binary["build_commit_status"] = "unavailable";
        if (path.empty() || _wcsicmp(path.filename().c_str(), expectedName) != 0)
        {
            Error(report, "binary", "path_or_role_unavailable");
            return binary;
        }
        if (GetDriveTypeW(path.root_path().c_str()) != DRIVE_FIXED)
        {
            Error(report, "binary", "nonlocal_image_omitted");
            return binary;
        }
        wil::unique_handle file{ CreateFileW(path.c_str(), GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING, FILE_FLAG_SEQUENTIAL_SCAN, nullptr) };
        LARGE_INTEGER size{};
        if (!file || !GetFileSizeEx(file.get(), &size) || size.QuadPart <= 0 || size.QuadPart > 256 * 1024 * 1024)
        {
            Error(report, "binary", "file_unavailable_or_size_limit");
            return binary;
        }
        ReadImageMetadata(file.get(), binary, report, _wcsicmp(expectedName, L"wta.exe") == 0);
        if (!binary.isMember("architecture"))
            return binary;
        LARGE_INTEGER beginning{};
        if (!SetFilePointerEx(file.get(), beginning, nullptr, FILE_BEGIN))
        {
            Error(report, "binary_hash", "seek_failed");
            return binary;
        }
        BCRYPT_ALG_HANDLE algorithm{};
        if (BCryptOpenAlgorithmProvider(&algorithm, BCRYPT_SHA256_ALGORITHM, nullptr, 0) < 0)
        {
            Error(report, "binary_hash", "algorithm_unavailable");
            return binary;
        }
        const auto closeAlgorithm = wil::scope_exit([&] { BCryptCloseAlgorithmProvider(algorithm, 0); });
        BCRYPT_HASH_HANDLE hash{};
        if (BCryptCreateHash(algorithm, &hash, nullptr, 0, nullptr, 0, 0) < 0)
        {
            Error(report, "binary_hash", "hash_unavailable");
            return binary;
        }
        const auto closeHash = wil::scope_exit([&] { BCryptDestroyHash(hash); });
        std::vector<BYTE> buffer(64 * 1024);
        const auto started = GetTickCount64();
        uint64_t total = 0;
        for (;;)
        {
            DWORD read{};
            if (GetTickCount64() - started > 2000 || !ReadFile(file.get(), buffer.data(), static_cast<DWORD>(buffer.size()), &read, nullptr))
            {
                Error(report, "binary_hash", "read_failed_or_time_limit");
                return binary;
            }
            if (!read)
                break;
            total += read;
            if (total > 256 * 1024 * 1024 || BCryptHashData(hash, buffer.data(), read, 0) < 0)
            {
                Error(report, "binary_hash", "hash_failed_or_size_limit");
                return binary;
            }
        }
        BYTE digest[32]{};
        if (total != static_cast<uint64_t>(size.QuadPart) || BCryptFinishHash(hash, digest, sizeof(digest), 0) < 0)
        {
            Error(report, "binary_hash", "inconsistent_file_or_hash_failed");
            return binary;
        }
        std::string hex;
        for (const auto byte : digest)
        {
            hex += "0123456789abcdef"[byte >> 4];
            hex += "0123456789abcdef"[byte & 15];
        }
        binary["sha256"] = std::move(hex);
        binary["size_bytes"] = Json::UInt64{ total };
        binary["product_version"] = ProductVersion(path);
        if (binary["product_version"].isNull())
            Error(report, "binary_version", "version_resource_unavailable");
        binary["status"] = "collected";
        binary["measured_utc"] = UtcNow();
        return binary;
    }

    void CollectBackground(Report& report)
    {
        auto& value = report.value;
        value["processes"] = Json::Value{ Json::arrayValue };
        value["binaries"] = Json::Value{ Json::arrayValue };
        value["environment"] = EnvironmentProvenance();
        value["build_brand"] = BuildBrand();
        value["expected_com_clsid"] = "{" __CLSID_TerminalProtocolServer "}";
        value["com_provenance"] = "authenticate_and_request_logs_authoritative_no_collection_activation";
        const auto root = ProcessPath(GetCurrentProcess()).parent_path();
        std::map<std::filesystem::path, Json::Value> measured;
        const auto addBinary = [&](const std::filesystem::path& path, const char* role, const wchar_t* name, const char* source) {
            auto found = measured.find(path);
            if (found == measured.end())
            {
                if (measured.size() >= 16)
                {
                    Error(value, "binary", "unique_image_limit");
                    return Json::Value{};
                }
                found = measured.emplace(path, HashProduct(path, name, value)).first;
            }
            auto binary = found->second;
            binary["role"] = role;
            binary["source"] = source;
            binary["path_category"] = !root.empty() && path.parent_path() == root ? "terminal_image_directory" : "outside_terminal_image_directory";
            const auto index = value["binaries"].size();
            value["binaries"].append(std::move(binary));
            return Json::Value{ index };
        };
        for (const auto& snapshot : report.processes)
        {
            try
            {
                auto process = snapshot.owner;
                if (!snapshot.process)
                {
                    process["identity"]["status"] = "unavailable";
                    value["processes"].append(std::move(process));
                    continue;
                }
                process["identity"] = ProcessIdentity(snapshot.process.get());
                if (process["identity"]["status"] != "available")
                    Error(value, "process_identity", "unavailable");
                if (process["identity"]["package_status"] == "unavailable")
                    Error(value, "process_package", "unavailable");
                const auto wait = WaitForSingleObject(snapshot.process.get(), 0);
                process["liveness"] = wait == WAIT_TIMEOUT ? "running" : wait == WAIT_OBJECT_0 ? "exited" : "unavailable";
                USHORT machine{}, native{};
                if (IsWow64Process2(snapshot.process.get(), &machine, &native))
                    process["architecture"] = Architecture(machine ? machine : native);
                else
                    Error(value, "process_architecture", "unavailable");
                const auto terminal = process["role"] == "terminal";
                process["binary_index"] = addBinary(ProcessPath(snapshot.process.get()), terminal ? "terminal" : "wta", terminal ? L"WindowsTerminal.exe" : L"wta.exe", "pinned_process_image");
                value["processes"].append(std::move(process));
            }
            catch (...)
            {
                Error(value, "process_collection", "unavailable");
            }
        }
        if (!root.empty())
        {
            addBinary(root / L"wta.exe", "wta", L"wta.exe", "installed_sibling_not_running_identity");
            addBinary(root / L"wtcli.exe", "wtcli", L"wtcli.exe", "installed_sibling_not_running_identity");
        }
        else
            Error(value, "installed_images", "install_root_unavailable");
        if (const auto proxy = GetModuleHandleW(L"OpenConsoleProxy.dll"))
        {
            std::wstring path(32768, L'\0');
            const auto length = GetModuleFileNameW(proxy, path.data(), static_cast<DWORD>(path.size()));
            if (length && length < path.size())
            {
                path.resize(length);
                addBinary(path, "com_proxy", L"OpenConsoleProxy.dll", "loaded_module");
            }
            else
                Error(value, "com_proxy", "module_path_unavailable");
        }
        else
            Error(value, "com_proxy", "not_loaded_in_collecting_process");
        value["collection_completed_utc"] = UtcNow();
    }

}
