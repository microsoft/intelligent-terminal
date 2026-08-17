// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include <unknwn.h>
#include <winrt/Windows.Foundation.h>

#include "Formatting.h"
#include "AppExtensionProviderCatalog.h"
#include "CommandRunner.h"
#include "ProviderContracts.h"
#include "ProviderRegistry.h"
#include "wtcli_functions.h"

// Classic-COM Terminal protocol. Generated from
// src/host/proxy/ITerminalProtocol.idl; found via the OpenConsoleProxy IntDir
// added to this project's include path. Marshaled by the OpenConsoleProxy
// proxy/stub (NOT WinRT MBM), so activation/marshaling never hits the combase
// WinRT activation catalog.
#include "ITerminalProtocol.h"

#include <CLI/CLI.hpp>

#include <wil/resource.h>

#include <algorithm>
#include <array>
#include <charconv>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <functional>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <sstream>
#include <string>
#include <thread>
#include <type_traits>
#include <vector>

// ── EventSink — pure classic-COM event sink for `listen` ──
struct EventSink : ITerminalProtocolEventSink
{
    LONG _ref{ 1 };
    std::function<void(const std::string&)> _handler;

    explicit EventSink(std::function<void(const std::string&)> handler) :
        _handler(std::move(handler)) {}

    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID riid, void** ppv) override
    {
        if (!ppv)
            return E_POINTER;
        if (riid == __uuidof(IUnknown) || riid == __uuidof(ITerminalProtocolEventSink))
        {
            *ppv = static_cast<ITerminalProtocolEventSink*>(this);
            AddRef();
            return S_OK;
        }
        *ppv = nullptr;
        return E_NOINTERFACE;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return InterlockedIncrement(&_ref); }
    ULONG STDMETHODCALLTYPE Release() override
    {
        const auto r = InterlockedDecrement(&_ref);
        if (r == 0)
            delete this;
        return r;
    }
    HRESULT STDMETHODCALLTYPE OnEvent(BSTR eventJson) override
    {
        if (_handler)
            _handler(eventJson ? winrt::to_string(winrt::hstring{ eventJson }) : std::string{});
        return S_OK;
    }
};

// ── Helpers ──

static winrt::com_ptr<ITerminalProtocol> ConnectToTerminal(bool* outAuthenticated = nullptr,
                                                          std::string* outVersion = nullptr,
                                                          bool skipAuthenticate = false)
{
    if (outAuthenticated)
        *outAuthenticated = false;
    if (outVersion)
        outVersion->clear();

    wchar_t clsid[128]{};
    if (!GetEnvironmentVariableW(L"WT_COM_CLSID", clsid, ARRAYSIZE(clsid)))
    {
        fprintf(stderr, "[wtcli] WT_COM_CLSID not set. Must run inside a Windows Terminal pane.\n");
        return nullptr;
    }

    CLSID cls{};
    if (FAILED(CLSIDFromString(clsid, &cls)))
    {
        fprintf(stderr, "[wtcli] Invalid CLSID: %ls\n", clsid);
        return nullptr;
    }

    winrt::com_ptr<ITerminalProtocol> server;
    auto hr = CoCreateInstance(cls, nullptr, CLSCTX_LOCAL_SERVER, __uuidof(ITerminalProtocol), server.put_void());
    if (FAILED(hr))
    {
        fprintf(stderr, "[wtcli] Connection failed: 0x%08X\n", static_cast<uint32_t>(hr));
        return nullptr;
    }
    if (skipAuthenticate)
    {
        return server;
    }

    BSTR rawAuth = nullptr;
    hr = server->Authenticate(nullptr, &rawAuth);
    bool parsed = false;
    bool authenticated = false;
    std::string version;
    if (SUCCEEDED(hr) && rawAuth)
    {
        Json::Value v;
        Json::CharReaderBuilder rb;
        std::string errs;
        auto s = winrt::to_string(winrt::hstring{ rawAuth });
        std::istringstream ss(s);
        if (Json::parseFromStream(rb, ss, &v, &errs))
        {
            parsed = true;
            authenticated = v["authenticated"].asBool();
            version = v["protocol_version"].asString();
        }
    }
    if (rawAuth)
        SysFreeString(rawAuth);

    if (FAILED(hr))
    {
        fprintf(stderr, "[wtcli] Authentication failed: 0x%08X\n", static_cast<uint32_t>(hr));
        return nullptr;
    }
    if (!parsed)
    {
        // Success HRESULT but a null/malformed auth payload is a broken
        // server contract — don't misreport it as a server rejection.
        fprintf(stderr, "[wtcli] Authentication response missing or malformed (server contract error)\n");
        return nullptr;
    }
    if (!authenticated)
    {
        fprintf(stderr, "[wtcli] Authentication rejected by server\n");
        return nullptr;
    }

    if (outAuthenticated)
        *outAuthenticated = authenticated;
    if (outVersion)
        *outVersion = version;
    return server;
}

static winrt::com_ptr<IRichTabPublisher> ConnectToRichTabPublisher()
{
    wchar_t clsid[128]{};
    if (!GetEnvironmentVariableW(L"WT_COM_CLSID", clsid, ARRAYSIZE(clsid)))
    {
        fprintf(stderr, "[wtcli] WT_COM_CLSID not set.\n");
        return nullptr;
    }

    CLSID cls{};
    if (FAILED(CLSIDFromString(clsid, &cls)))
    {
        fprintf(stderr, "[wtcli] Invalid CLSID: %ls\n", clsid);
        return nullptr;
    }

    winrt::com_ptr<IRichTabPublisher> publisher;
    const auto hr = CoCreateInstance(cls, nullptr, CLSCTX_LOCAL_SERVER, __uuidof(IRichTabPublisher), publisher.put_void());
    if (FAILED(hr))
    {
        fprintf(stderr, "[wtcli] Rich Tab publisher connection failed: 0x%08X\n", static_cast<uint32_t>(hr));
        return nullptr;
    }
    return publisher;
}

static bool InvokeRichTabPublisher(
    IRichTabPublisher* publisher,
    const Json::Value& request,
    Json::Value& result)
{
    Json::StreamWriterBuilder writer;
    writer["indentation"] = "";
    const auto requestJson = winrt::to_hstring(Json::writeString(writer, request));
    wil::unique_bstr requestBstr{ ::SysAllocString(requestJson.c_str()) };
    if (!requestBstr)
    {
        fprintf(stderr, "[wtcli] Failed to allocate Rich Tab publisher request.\n");
        return false;
    }

    BSTR rawResult = nullptr;
    const auto hr = publisher->Invoke(requestBstr.get(), &rawResult);
    wil::unique_bstr resultBstr{ rawResult };
    if (FAILED(hr) || !resultBstr)
    {
        fprintf(stderr, "[wtcli] Rich Tab publisher call failed: 0x%08X\n", static_cast<uint32_t>(hr));
        return false;
    }

    Json::CharReaderBuilder reader;
    std::string errors;
    std::istringstream stream{ winrt::to_string(winrt::hstring{ resultBstr.get() }) };
    if (!Json::parseFromStream(reader, stream, &result, &errors) || !result.isObject())
    {
        fprintf(stderr, "[wtcli] Rich Tab publisher returned malformed JSON.\n");
        return false;
    }
    return true;
}

static std::optional<std::string> ReadBoundedStdin(const size_t maximumSize)
{
    std::string input;
    std::array<char, 4096> buffer{};
    for (;;)
    {
        const auto read = fread(buffer.data(), 1, buffer.size(), stdin);
        if (read != 0)
        {
            if (input.size() + read > maximumSize)
            {
                return std::nullopt;
            }
            input.append(buffer.data(), read);
        }
        if (read != buffer.size())
        {
            if (ferror(stdin))
            {
                return std::nullopt;
            }
            return input;
        }
    }
}

// Call a method that returns a JSON BSTR; parse into `out`. Returns the HRESULT.
template<typename F>
static HRESULT CallJson(F&& call, Json::Value& out)
{
    BSTR raw = nullptr;
    HRESULT hr = call(&raw);
    if (SUCCEEDED(hr))
    {
        bool parsed = false;
        if (raw)
        {
            Json::CharReaderBuilder rb;
            std::string errs;
            auto s = winrt::to_string(winrt::hstring{ raw });
            std::istringstream ss(s);
            parsed = Json::parseFromStream(rb, ss, &out, &errs);
        }
        // A success HRESULT with a null BSTR or malformed JSON is a broken
        // server contract; surface it as an error so callers' FAILED(hr)
        // checks fire immediately instead of proceeding with a
        // default-constructed `out`.
        if (!parsed)
            hr = E_UNEXPECTED;
    }
    if (raw)
        SysFreeString(raw);
    return hr;
}

static std::string GuidToString(const GUID& g)
{
    wchar_t buf[40]{};
    StringFromGUID2(g, buf, ARRAYSIZE(buf));
    std::wstring ws(buf);
    if (ws.size() > 2 && ws.front() == L'{' && ws.back() == L'}')
        ws = ws.substr(1, ws.size() - 2);
    return winrt::to_string(winrt::hstring{ ws });
}

static GUID GuidFromString(const std::string& target)
{
    auto wstr = winrt::to_hstring(target);
    std::wstring guidStr{ wstr };
    if (!guidStr.empty() && guidStr[0] != L'{')
        guidStr = L"{" + guidStr + L"}";
    GUID g{};
    if (FAILED(CLSIDFromString(guidStr.c_str(), &g)))
    {
        if (!target.empty())
            fprintf(stderr, "[wtcli] Invalid session ID: %s\n", target.c_str());
        return GUID{};
    }
    return g;
}

// Resolve a session id: an explicit GUID string, or the active pane's id.
static GUID ResolveSessionId(ITerminalProtocol* server, const std::string& target)
{
    if (!target.empty())
        return GuidFromString(target);

    Json::Value info;
    const auto hr = CallJson([&](BSTR* j) { return server->GetActivePane(j); }, info);
    if (FAILED(hr))
    {
        fprintf(stderr, "[wtcli] Could not resolve active pane (GetActivePane failed: 0x%08X)\n", static_cast<uint32_t>(hr));
        return GUID{};
    }
    const auto sessionId = info["session_id"].asString();
    if (sessionId.empty())
    {
        fprintf(stderr, "[wtcli] No active pane.\n");
        return GUID{};
    }
    return GuidFromString(sessionId);
}

static uint64_t GetFirstWindowId(ITerminalProtocol* server)
{
    Json::Value windows;
    CallJson([&](BSTR* j) { return server->ListWindows(j); }, windows);
    if (windows.isArray() && !windows.empty())
        return windows[0u]["window_id"].asUInt64();
    return 0;
}

static uint32_t GetFirstTabId(ITerminalProtocol* server, uint64_t windowId)
{
    Json::Value tabs;
    CallJson([&](BSTR* j) { return server->ListTabs(windowId, j); }, tabs);
    if (tabs.isArray() && !tabs.empty())
        return tabs[0u]["tab_id"].asUInt();
    return UINT32_MAX;
}

// Allocate a BSTR from a UTF-8 std::string.
static BSTR Bstr(const std::string& s)
{
    return SysAllocString(winrt::to_hstring(s).c_str());
}

// Parse a base-10 unsigned 64-bit integer without throwing (unlike std::stoull,
// which aborts wtcli on non-numeric input). Returns false on empty, non-numeric,
// trailing-garbage, or overflowing input.
static bool TryParseU64(const std::string& s, uint64_t& out)
{
    if (s.empty())
        return false;
    uint64_t v = 0;
    const auto* first = s.data();
    const auto* last = s.data() + s.size();
    const auto [ptr, ec] = std::from_chars(first, last, v);
    if (ec != std::errc{} || ptr != last)
        return false;
    out = v;
    return true;
}

static bool TryParseTtl(const std::string& value, uint64_t& milliseconds)
{
    if (value.size() < 2)
    {
        return false;
    }
    uint64_t multiplier = 0;
    const auto suffix = value.back();
    switch (suffix)
    {
    case 's':
        multiplier = 1000;
        break;
    case 'm':
        multiplier = 60 * 1000;
        break;
    case 'h':
        multiplier = 60 * 60 * 1000;
        break;
    default:
        return false;
    }
    uint64_t amount = 0;
    if (!TryParseU64(value.substr(0, value.size() - 1), amount) ||
        amount == 0 ||
        amount > std::chrono::duration_cast<std::chrono::hours>(std::chrono::hours{ 24 * 7 }).count() * 60 * 60 / (multiplier / 1000))
    {
        return false;
    }
    milliseconds = amount * multiplier;
    return true;
}

static std::optional<std::string> ReadBoundedFile(
    const std::filesystem::path& path,
    const size_t maximumSize,
    std::string& error)
{
    std::error_code ec;
    const auto size = std::filesystem::file_size(path, ec);
    if (ec)
    {
        error = "Could not read file size: " + std::to_string(ec.value());
        return std::nullopt;
    }
    if (size == 0 || size > maximumSize)
    {
        error = size == 0 ? "File is empty" : "File exceeds the size limit";
        return std::nullopt;
    }

    std::ifstream stream{ path, std::ios::binary };
    if (!stream)
    {
        error = "Could not open file";
        return std::nullopt;
    }
    std::string contents(static_cast<size_t>(size), '\0');
    stream.read(contents.data(), static_cast<std::streamsize>(contents.size()));
    if (!stream || stream.peek() != std::ifstream::traits_type::eof())
    {
        error = "Could not read the complete file";
        return std::nullopt;
    }
    return contents;
}

static std::optional<std::filesystem::path> AbsoluteUtf8Path(
    const std::string& value,
    std::string& error)
{
    std::filesystem::path input;
    try
    {
        input = std::filesystem::path{ winrt::to_hstring(value).c_str() };
    }
    catch (const winrt::hresult_error& conversionError)
    {
        char buffer[64]{};
        sprintf_s(buffer, "Invalid UTF-8 path (0x%08X)", static_cast<uint32_t>(conversionError.code()));
        error = buffer;
        return std::nullopt;
    }
    std::error_code pathError;
    auto absolute = std::filesystem::absolute(input, pathError);
    if (pathError)
    {
        error = "Invalid path: " + std::to_string(pathError.value());
        return std::nullopt;
    }
    return absolute;
}

static Json::Value RegistrationJson(
    const Microsoft::Terminal::RichTab::Provider::Registration& registration)
{
    namespace Provider = Microsoft::Terminal::RichTab::Provider;
    Json::Value value;
    value["id"] = registration.manifest.id;
    value["display_name"] = registration.manifest.displayName;
    value["publisher"] = registration.manifest.publisher;
    value["version"] = registration.manifest.version;
    switch (registration.sourceIdentity.kind)
    {
    case Provider::ProviderSourceKind::BuiltIn:
        value["source"] = "built-in";
        break;
    case Provider::ProviderSourceKind::AppExtension:
        value["source"] = "app-extension";
        value["package_family_name"] = winrt::to_string(
            winrt::hstring{ registration.sourceIdentity.packageFamilyName });
        value["extension_id"] = winrt::to_string(
            winrt::hstring{ registration.sourceIdentity.extensionId });
        break;
    case Provider::ProviderSourceKind::Development:
        value["source"] = "development";
        break;
    default:
        value["source"] = "legacy-managed";
        break;
    }
    value["enabled"] = registration.enabled;
    value["integrity_valid"] = registration.integrityValid;
    value["payload_hash"] = registration.payloadHash;
    value["root"] = winrt::to_string(winrt::hstring{ registration.root.c_str() });
    return value;
}

static void PrintRegistration(
    const Microsoft::Terminal::RichTab::Provider::Registration& registration)
{
    namespace Provider = Microsoft::Terminal::RichTab::Provider;
    const auto source =
        registration.sourceIdentity.kind == Provider::ProviderSourceKind::BuiltIn ? "built-in    " :
        registration.sourceIdentity.kind == Provider::ProviderSourceKind::AppExtension ? "app-extension" :
        registration.sourceIdentity.kind == Provider::ProviderSourceKind::Development ? "development " :
                                                                                         "legacy-managed";
    printf(
        "%s  %s  %s  %s\n",
        registration.enabled ? "enabled " : "disabled",
        source,
        registration.integrityValid ? "verified" : "CHANGED ",
        registration.manifest.id.c_str());
}

static std::vector<Microsoft::Terminal::RichTab::Provider::Registration>
DiscoverAppExtensionRegistrations(
    Microsoft::Terminal::RichTab::Provider::ProviderRegistry& registry,
    std::vector<std::string>& diagnostics,
    bool& catalogAvailable)
{
    namespace Provider = Microsoft::Terminal::RichTab::Provider;
    auto discovery = Provider::AppExtensionProviderCatalog::DiscoverAsync().get();
    catalogAvailable = discovery.diagnostics.empty();
    diagnostics.insert(
        diagnostics.end(),
        discovery.diagnostics.begin(),
        discovery.diagnostics.end());

    std::vector<Provider::Registration> registrations;
    for (auto& discovered : discovery.providers)
    {
        if (discovered.status != Provider::AppExtensionDiscoveryStatus::Discovered ||
            !discovered.manifest)
        {
            diagnostics.insert(
                diagnostics.end(),
                discovered.diagnostics.begin(),
                discovered.diagnostics.end());
            continue;
        }
        const auto consent = registry.AppExtensionConsentEnabled(
            discovered.identity);
        diagnostics.insert(
            diagnostics.end(),
            consent.errors.begin(),
            consent.errors.end());

        Provider::Registration registration;
        registration.manifest = std::move(*discovered.manifest);
        registration.kind = Provider::RegistrationKind::Managed;
        registration.root = std::move(discovered.publicPath);
        registration.enabled = consent.value.value_or(false);
        registration.integrityValid = true;
        registration.sourceIdentity = std::move(discovered.identity);
        registrations.emplace_back(std::move(registration));
    }
    return registrations;
}

// ── Main ──

int main()
{
    winrt::init_apartment(winrt::apartment_type::multi_threaded);

    CLI::App app{ "wtcli - Windows Terminal CLI" };
    app.require_subcommand(0, 1);

    bool jsonMode = false;
    bool skipAuthenticate = false;
    int exitCode = 0;
    app.add_flag("--json", jsonMode, "Output raw JSON");
    app.add_flag("--skip-authenticate", skipAuthenticate, "Skip the compatibility handshake (testing only)");

    auto connect = [&]() -> winrt::com_ptr<ITerminalProtocol> {
        auto server = ConnectToTerminal(nullptr, nullptr, skipAuthenticate);
        if (!server)
            exitCode = 1;
        return server;
    };

    // ── provider ──
    // Provider management is intentionally local and must never require a
    // running Terminal or WT_COM_CLSID.
    auto* providerCmd = app.add_subcommand("provider", "Manage Rich Tab providers");
    providerCmd->require_subcommand(1, 1);
    auto* providerPublishCmd = providerCmd->add_subcommand("publish", "Publish a Rich Tab Provider Snapshot");
    bool providerPublishStdin = false;
    providerPublishCmd->add_flag("--stdin", providerPublishStdin, "Read the Snapshot JSON from stdin")->required();
    providerPublishCmd->callback([&]() {
        const auto snapshotText = ReadBoundedStdin(Microsoft::Terminal::RichTab::Provider::MaximumResponseSize);
        if (!snapshotText)
        {
            fprintf(stderr, "[wtcli] provider publish: stdin exceeds the size limit or could not be read\n");
            exitCode = 1;
            return;
        }

        Json::Value snapshot;
        Json::CharReaderBuilder reader;
        std::string errors;
        std::istringstream stream{ *snapshotText };
        if (!Json::parseFromStream(reader, stream, &snapshot, &errors) || !snapshot.isObject())
        {
            fprintf(stderr, "[wtcli] provider publish: stdin must contain one JSON object\n");
            exitCode = 1;
            return;
        }

        wchar_t lease[128]{};
        if (!GetEnvironmentVariableW(L"WT_RICH_TAB_LEASE", lease, ARRAYSIZE(lease)))
        {
            fprintf(stderr, "[wtcli] provider publish: WT_RICH_TAB_LEASE is not set\n");
            exitCode = 1;
            return;
        }
        auto publisher = ConnectToRichTabPublisher();
        if (!publisher)
        {
            exitCode = 1;
            return;
        }

        Json::Value request;
        request["operation"] = "publish";
        request["lease"] = winrt::to_string(winrt::hstring{ lease });
        request["snapshot"] = std::move(snapshot);
        Json::Value result;
        if (!InvokeRichTabPublisher(publisher.get(), request, result) || !result["ok"].asBool())
        {
            fprintf(stderr, "[wtcli] provider publish: %s\n", result["message"].asCString());
            exitCode = 1;
            return;
        }
        Json::StreamWriterBuilder writer;
        writer["indentation"] = "";
        printf("%s\n", Json::writeString(writer, result).c_str());
    });

    auto* tabCmd = app.add_subcommand("tab", "Manage tabs");
    tabCmd->require_subcommand(1, 1);
    auto* tabMetadataCmd = tabCmd->add_subcommand("metadata", "Manage a tab's second-line metadata");
    tabMetadataCmd->require_subcommand(1, 1);

    std::string metadataSetTabId;
    std::string metadataSetText;
    std::string metadataSetTooltip;
    std::string metadataSetAccessibilityText;
    std::string metadataSetTtl;
    auto* metadataSetCmd = tabMetadataCmd->add_subcommand("set", "Override the complete second line");
    metadataSetCmd->add_option("--tab-id", metadataSetTabId, "Stable tab ID from list-tabs")->required();
    metadataSetCmd->add_option("--text", metadataSetText, "Second-line text")->required();
    metadataSetCmd->add_option("--tooltip", metadataSetTooltip, "Tooltip text");
    metadataSetCmd->add_option("--accessibility-text", metadataSetAccessibilityText, "Accessibility text");
    metadataSetCmd->add_option("--ttl", metadataSetTtl, "Expiry such as 30s, 5m, or 1h");
    metadataSetCmd->callback([&]() {
        if (metadataSetText.empty() ||
            metadataSetText.size() > Microsoft::Terminal::RichTab::Provider::MaximumPresentationTextSize ||
            metadataSetTooltip.size() > Microsoft::Terminal::RichTab::Provider::MaximumPresentationTextSize ||
            metadataSetAccessibilityText.size() > Microsoft::Terminal::RichTab::Provider::MaximumPresentationTextSize)
        {
            fprintf(stderr, "[wtcli] tab metadata set: text values are empty or exceed the size limit\n");
            exitCode = 1;
            return;
        }
        uint64_t ttlMilliseconds = 0;
        if (!metadataSetTtl.empty() && !TryParseTtl(metadataSetTtl, ttlMilliseconds))
        {
            fprintf(stderr, "[wtcli] tab metadata set: --ttl must be 1s..168h\n");
            exitCode = 1;
            return;
        }
        auto publisher = ConnectToRichTabPublisher();
        if (!publisher)
        {
            exitCode = 1;
            return;
        }
        Json::Value request;
        request["operation"] = "metadata_set";
        request["tab_id"] = metadataSetTabId;
        request["text"] = metadataSetText;
        request["tooltip"] = metadataSetTooltip.empty() ? metadataSetText : metadataSetTooltip;
        request["accessibility_text"] = metadataSetAccessibilityText.empty() ? metadataSetText : metadataSetAccessibilityText;
        request["ttl_milliseconds"] = Json::UInt64{ ttlMilliseconds };
        Json::Value result;
        if (!InvokeRichTabPublisher(publisher.get(), request, result) || !result["ok"].asBool())
        {
            fprintf(stderr, "[wtcli] tab metadata set: %s\n", result["message"].asCString());
            exitCode = 1;
        }
    });

    std::string metadataClearTabId;
    auto* metadataClearCmd = tabMetadataCmd->add_subcommand("clear", "Clear the second-line override");
    metadataClearCmd->add_option("--tab-id", metadataClearTabId, "Stable tab ID from list-tabs")->required();
    metadataClearCmd->callback([&]() {
        auto publisher = ConnectToRichTabPublisher();
        if (!publisher)
        {
            exitCode = 1;
            return;
        }
        Json::Value request;
        request["operation"] = "metadata_clear";
        request["tab_id"] = metadataClearTabId;
        Json::Value result;
        if (!InvokeRichTabPublisher(publisher.get(), request, result) || !result["ok"].asBool())
        {
            fprintf(stderr, "[wtcli] tab metadata clear: %s\n", result["message"].asCString());
            exitCode = 1;
        }
    });

    std::string providerManifestPath;
    auto* providerValidateCmd = providerCmd->add_subcommand("validate", "Validate a provider manifest");
    providerValidateCmd->add_option("manifest", providerManifestPath, "Path to provider.json")->required();
    providerValidateCmd->callback([&]() {
        std::string pathError;
        const auto manifestPath = AbsoluteUtf8Path(providerManifestPath, pathError);
        if (!manifestPath)
        {
            fprintf(stderr, "[wtcli] provider validate: %s\n", pathError.c_str());
            exitCode = 1;
            return;
        }
        std::string readError;
        const auto contents = ReadBoundedFile(
            *manifestPath,
            Microsoft::Terminal::RichTab::Provider::MaximumManifestSize,
            readError);
        if (!contents)
        {
            fprintf(stderr, "[wtcli] provider validate: %s\n", readError.c_str());
            exitCode = 1;
            return;
        }

        const auto parsed = Microsoft::Terminal::RichTab::Provider::ParseManifest(
            *contents,
            manifestPath->parent_path());
        if (!parsed)
        {
            if (jsonMode)
            {
                Json::Value output;
                output["valid"] = false;
                output["errors"] = Json::arrayValue;
                for (const auto& error : parsed.errors)
                {
                    output["errors"].append(error);
                }
                PrintJson(output);
            }
            else
            {
                for (const auto& error : parsed.errors)
                {
                    fprintf(stderr, "[wtcli] %s\n", error.c_str());
                }
            }
            exitCode = 1;
            return;
        }

        if (jsonMode)
        {
            Json::Value output;
            output["valid"] = true;
            output["id"] = parsed.value->id;
            output["version"] = parsed.value->version;
            output["field_count"] = static_cast<Json::UInt>(parsed.value->fields.size());
            PrintJson(output);
        }
        else
        {
            printf(
                "Valid provider manifest: %s (%s), %zu fields\n",
                parsed.value->id.c_str(),
                parsed.value->version.c_str(),
                parsed.value->fields.size());
        }
    });

    std::string providerInstallManifestPath;
    auto* providerInstallCmd = providerCmd->add_subcommand("install", "Install a managed provider");
    providerInstallCmd->add_option("manifest", providerInstallManifestPath, "Path to provider.json")->required();
    providerInstallCmd->callback([&]() {
        std::string pathError;
        const auto manifestPath = AbsoluteUtf8Path(providerInstallManifestPath, pathError);
        if (!manifestPath)
        {
            fprintf(stderr, "[wtcli] provider install: %s\n", pathError.c_str());
            exitCode = 1;
            return;
        }
        const auto installed = Microsoft::Terminal::RichTab::Provider::ProviderRegistry{}.Install(*manifestPath);
        if (!installed)
        {
            for (const auto& error : installed.errors)
                fprintf(stderr, "[wtcli] %s\n", error.c_str());
            exitCode = 1;
            return;
        }
        if (jsonMode)
            PrintJson(RegistrationJson(*installed.value));
        else
        {
            printf("Installed provider (disabled until explicitly enabled):\n");
            PrintRegistration(*installed.value);
        }
    });

    std::string providerRegisterManifestPath;
    bool providerRegisterDevelopment = false;
    auto* providerRegisterCmd = providerCmd->add_subcommand("register", "Register a provider source directory");
    providerRegisterCmd->add_flag("--dev", providerRegisterDevelopment, "Register mutable development code without copying")->required();
    providerRegisterCmd->add_option("manifest", providerRegisterManifestPath, "Path to provider.json")->required();
    providerRegisterCmd->callback([&]() {
        std::string pathError;
        const auto manifestPath = AbsoluteUtf8Path(providerRegisterManifestPath, pathError);
        if (!manifestPath)
        {
            fprintf(stderr, "[wtcli] provider register: %s\n", pathError.c_str());
            exitCode = 1;
            return;
        }
        const auto registered = Microsoft::Terminal::RichTab::Provider::ProviderRegistry{}.RegisterDevelopment(*manifestPath);
        if (!registered)
        {
            for (const auto& error : registered.errors)
                fprintf(stderr, "[wtcli] %s\n", error.c_str());
            exitCode = 1;
            return;
        }
        if (jsonMode)
            PrintJson(RegistrationJson(*registered.value));
        else
        {
            printf("Registered mutable development provider (disabled until explicitly enabled):\n");
            PrintRegistration(*registered.value);
        }
    });

    auto* providerListCmd = providerCmd->add_subcommand("list", "List discovered and registered providers");
    providerListCmd->callback([&]() {
        namespace Provider = Microsoft::Terminal::RichTab::Provider;
        Provider::ProviderRegistry registry;
        auto registrations = registry.List();
        if (!registrations)
        {
            for (const auto& error : registrations.errors)
                fprintf(stderr, "[wtcli] %s\n", error.c_str());
            exitCode = 1;
            return;
        }
        bool catalogAvailable = false;
        auto appExtensions = DiscoverAppExtensionRegistrations(
            registry,
            registrations.errors,
            catalogAvailable);
        registrations.value->insert(
            registrations.value->end(),
            std::make_move_iterator(appExtensions.begin()),
            std::make_move_iterator(appExtensions.end()));
        if (jsonMode)
        {
            Json::Value output;
            output["providers"] = Json::arrayValue;
            for (const auto& registration : *registrations.value)
                output["providers"].append(RegistrationJson(registration));
            output["warnings"] = Json::arrayValue;
            for (const auto& warning : registrations.errors)
                output["warnings"].append(warning);
            PrintJson(output);
        }
        else if (registrations.value->empty())
        {
            printf("No Rich Tab providers are registered.\n");
        }
        else
        {
            for (const auto& registration : *registrations.value)
                PrintRegistration(registration);
            for (const auto& warning : registrations.errors)
                fprintf(stderr, "[wtcli] warning: %s\n", warning.c_str());
        }
        if (!registrations.errors.empty())
            exitCode = 1;
    });

    std::string providerEnableId;
    bool providerAcceptCodeExecution = false;
    auto* providerEnableCmd = providerCmd->add_subcommand("enable", "Enable a provider");
    providerEnableCmd->add_option("id", providerEnableId, "Provider id")->required();
    providerEnableCmd->add_flag(
        "--accept-code-execution",
        providerAcceptCodeExecution,
        "Confirm that enabling the provider executes code as the current user");
    providerEnableCmd->callback([&]() {
        namespace Provider = Microsoft::Terminal::RichTab::Provider;
        if (!providerAcceptCodeExecution)
        {
            fprintf(
                stderr,
                "[wtcli] Enabling a provider executes code with your user permissions. "
                "Pass --accept-code-execution to confirm.\n");
            exitCode = 1;
            return;
        }
        Provider::ProviderRegistry registry;
        std::vector<std::string> discoveryDiagnostics;
        bool catalogAvailable = false;
        auto appExtensions = DiscoverAppExtensionRegistrations(
            registry,
            discoveryDiagnostics,
            catalogAvailable);
        const auto appExtension = std::find_if(
            appExtensions.begin(),
            appExtensions.end(),
            [&](const auto& registration) {
                return registration.manifest.id == providerEnableId;
            });
        if (appExtension != appExtensions.end())
        {
            const auto consent = registry.SetAppExtensionConsentEnabled(
                appExtension->sourceIdentity,
                true);
            if (!consent)
            {
                for (const auto& error : consent.errors)
                    fprintf(stderr, "[wtcli] %s\n", error.c_str());
                exitCode = 1;
                return;
            }
            appExtension->enabled = true;
            if (jsonMode)
                PrintJson(RegistrationJson(*appExtension));
            else
                PrintRegistration(*appExtension);
            return;
        }
        if (!catalogAvailable)
        {
            for (const auto& error : discoveryDiagnostics)
                fprintf(stderr, "[wtcli] App Extension discovery failed: %s\n", error.c_str());
            exitCode = 1;
            return;
        }

        const auto enabled = registry.SetEnabled(providerEnableId, true);
        if (!enabled)
        {
            for (const auto& error : discoveryDiagnostics)
                fprintf(stderr, "[wtcli] App Extension warning: %s\n", error.c_str());
            for (const auto& error : enabled.errors)
                fprintf(stderr, "[wtcli] %s\n", error.c_str());
            exitCode = 1;
            return;
        }
        if (jsonMode)
            PrintJson(RegistrationJson(*enabled.value));
        else
            PrintRegistration(*enabled.value);
    });

    std::string providerDisableId;
    auto* providerDisableCmd = providerCmd->add_subcommand("disable", "Disable a provider");
    providerDisableCmd->add_option("id", providerDisableId, "Provider id")->required();
    providerDisableCmd->callback([&]() {
        namespace Provider = Microsoft::Terminal::RichTab::Provider;
        Provider::ProviderRegistry registry;
        std::vector<std::string> discoveryDiagnostics;
        bool catalogAvailable = false;
        auto appExtensions = DiscoverAppExtensionRegistrations(
            registry,
            discoveryDiagnostics,
            catalogAvailable);
        const auto appExtension = std::find_if(
            appExtensions.begin(),
            appExtensions.end(),
            [&](const auto& registration) {
                return registration.manifest.id == providerDisableId;
            });
        if (appExtension != appExtensions.end())
        {
            const auto consent = registry.SetAppExtensionConsentEnabled(
                appExtension->sourceIdentity,
                false);
            if (!consent)
            {
                for (const auto& error : consent.errors)
                    fprintf(stderr, "[wtcli] %s\n", error.c_str());
                exitCode = 1;
                return;
            }
            appExtension->enabled = false;
            if (jsonMode)
                PrintJson(RegistrationJson(*appExtension));
            else
                PrintRegistration(*appExtension);
            return;
        }
        if (!catalogAvailable)
        {
            for (const auto& error : discoveryDiagnostics)
                fprintf(stderr, "[wtcli] App Extension discovery failed: %s\n", error.c_str());
            exitCode = 1;
            return;
        }

        const auto disabled = registry.SetEnabled(providerDisableId, false);
        if (!disabled)
        {
            for (const auto& error : discoveryDiagnostics)
                fprintf(stderr, "[wtcli] App Extension warning: %s\n", error.c_str());
            for (const auto& error : disabled.errors)
                fprintf(stderr, "[wtcli] %s\n", error.c_str());
            exitCode = 1;
            return;
        }
        if (jsonMode)
            PrintJson(RegistrationJson(*disabled.value));
        else
            PrintRegistration(*disabled.value);
    });

    std::string providerRemoveId;
    auto* providerRemoveCmd = providerCmd->add_subcommand("remove", "Remove a provider registration and managed payload");
    providerRemoveCmd->add_option("id", providerRemoveId, "Provider id")->required();
    providerRemoveCmd->callback([&]() {
        const auto removed = Microsoft::Terminal::RichTab::Provider::ProviderRegistry{}.Remove(providerRemoveId);
        if (!removed)
        {
            for (const auto& error : removed.errors)
                fprintf(stderr, "[wtcli] %s\n", error.c_str());
            exitCode = 1;
            return;
        }
        if (jsonMode)
        {
            Json::Value output;
            output["removed"] = true;
            output["id"] = providerRemoveId;
            PrintJson(output);
        }
        else
            printf("Removed provider %s.\n", providerRemoveId.c_str());
    });

    std::string providerTestManifestPath;
    std::string providerTestWorkingDirectory;
    int providerTestTimeoutSeconds = 10;
    auto* providerTestCmd = providerCmd->add_subcommand("test", "Run one provider refresh without a Terminal");
    providerTestCmd->add_option("manifest", providerTestManifestPath, "Path to provider.json")->required();
    providerTestCmd->add_option("--cwd", providerTestWorkingDirectory, "Working directory passed to the provider");
    providerTestCmd->add_option("--timeout", providerTestTimeoutSeconds, "Timeout in seconds")->check(CLI::Range(1, 300));
    providerTestCmd->callback([&]() {
        std::filesystem::path manifestPath;
        std::filesystem::path workingDirectory;
        std::string pathMessage;
        const auto resolvedManifest = AbsoluteUtf8Path(providerTestManifestPath, pathMessage);
        if (!resolvedManifest)
        {
            fprintf(stderr, "[wtcli] provider test: %s\n", pathMessage.c_str());
            exitCode = 1;
            return;
        }
        manifestPath = *resolvedManifest;
        if (providerTestWorkingDirectory.empty())
        {
            std::error_code pathError;
            workingDirectory = std::filesystem::current_path(pathError);
            if (pathError)
            {
                fprintf(stderr, "[wtcli] provider test: invalid working directory (%d)\n", pathError.value());
                exitCode = 1;
                return;
            }
        }
        else
        {
            const auto resolvedWorkingDirectory = AbsoluteUtf8Path(providerTestWorkingDirectory, pathMessage);
            if (!resolvedWorkingDirectory)
            {
                fprintf(stderr, "[wtcli] provider test: %s\n", pathMessage.c_str());
                exitCode = 1;
                return;
            }
            workingDirectory = *resolvedWorkingDirectory;
        }

        std::string readError;
        const auto contents = ReadBoundedFile(
            manifestPath,
            Microsoft::Terminal::RichTab::Provider::MaximumManifestSize,
            readError);
        if (!contents)
        {
            fprintf(stderr, "[wtcli] provider test: %s\n", readError.c_str());
            exitCode = 1;
            return;
        }
        const auto manifest = Microsoft::Terminal::RichTab::Provider::ParseManifest(
            *contents,
            manifestPath.parent_path());
        if (!manifest)
        {
            for (const auto& error : manifest.errors)
            {
                fprintf(stderr, "[wtcli] %s\n", error.c_str());
            }
            exitCode = 1;
            return;
        }

        Microsoft::Terminal::RichTab::Provider::Request request;
        request.requestId = "wtcli-test-" + std::to_string(GetCurrentProcessId()) + "-" + std::to_string(GetTickCount64());
        request.providerId = manifest.value->id;
        request.processEpoch = 1;
        request.sessionId = "wtcli-provider-test";
        request.reason = Microsoft::Terminal::RichTab::Provider::ActivationEvent::ManualRefresh;
        request.workingDirectory = workingDirectory;
        request.workingDirectoryAuthoritative = true;
        request.contextRevision = 1;
        const auto serialized = Microsoft::Terminal::RichTab::Provider::SerializeRequest(request, *manifest.value);
        if (!serialized)
        {
            for (const auto& error : serialized.errors)
            {
                fprintf(stderr, "[wtcli] %s\n", error.c_str());
            }
            exitCode = 1;
            return;
        }

        const auto commandResult = Microsoft::Terminal::RichTab::Provider::CommandRunner{}.Run(
            *manifest.value,
            *serialized.value,
            std::chrono::seconds{ providerTestTimeoutSeconds });
        if (commandResult.status != Microsoft::Terminal::RichTab::Provider::CommandResult::Status::Completed ||
            commandResult.exitCode != 0)
        {
            fprintf(
                stderr,
                "[wtcli] provider failed (status=%u, exit=%u, win32=%u)\n%s\n",
                static_cast<unsigned>(commandResult.status),
                commandResult.exitCode,
                commandResult.win32Error,
                commandResult.standardError.c_str());
            exitCode = 1;
            return;
        }

        const auto snapshot = Microsoft::Terminal::RichTab::Provider::ParseSnapshot(
            commandResult.standardOutput,
            *manifest.value,
            request.requestId);
        if (!snapshot)
        {
            for (const auto& error : snapshot.errors)
            {
                fprintf(stderr, "[wtcli] %s\n", error.c_str());
            }
            exitCode = 1;
            return;
        }

        if (jsonMode)
        {
            Json::Value output;
            output["provider_id"] = manifest.value->id;
            output["fields"] = Json::objectValue;
            for (const auto& [id, value] : snapshot.value->fields)
            {
                std::visit([&](const auto& fieldValue) {
                    output["fields"][id] = fieldValue;
                }, value);
            }
            if (snapshot.value->tooltip)
                output["tooltip"] = *snapshot.value->tooltip;
            if (snapshot.value->accessibilityText)
                output["accessibility_text"] = *snapshot.value->accessibilityText;
            PrintJson(output);
        }
        else if (snapshot.value->fields.empty())
        {
            printf("Provider returned an empty snapshot.\n");
        }
        else
        {
            printf("%s\n", manifest.value->displayName.c_str());
            for (const auto& field : manifest.value->fields)
            {
                if (const auto found = snapshot.value->fields.find(field.id); found != snapshot.value->fields.end())
                {
                    printf("  %s: ", field.displayName.c_str());
                    std::visit([](const auto& value) {
                        if constexpr (std::is_same_v<std::decay_t<decltype(value)>, std::string>)
                            printf("%s", value.c_str());
                        else if constexpr (std::is_same_v<std::decay_t<decltype(value)>, bool>)
                            printf("%s", value ? "true" : "false");
                        else if constexpr (std::is_same_v<std::decay_t<decltype(value)>, int64_t>)
                            printf("%lld", static_cast<long long>(value));
                        else
                            printf("%g", value);
                    }, found->second);
                    printf("\n");
                }
            }
        }
    });

    // ── list-windows ──
    auto* listWindowsCmd = app.add_subcommand("list-windows", "List all windows")->alias("lsw");
    listWindowsCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        Json::Value windows;
        auto hr = CallJson([&](BSTR* j) { return server->ListWindows(j); }, windows);
        if (FAILED(hr)) { fprintf(stderr, "ListWindows failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
        {
            Json::Value arr(Json::objectValue);
            arr["windows"] = windows;
            PrintJson(arr);
        }
        else
        {
            FormatWindowsHuman(windows);
        }
    });

    // ── list-tabs ──
    std::string listTabsWindowId;
    auto* listTabsCmd = app.add_subcommand("list-tabs", "List tabs in a window")->alias("lst");
    listTabsCmd->add_option("-w,--window-id", listTabsWindowId, "Window ID");
    listTabsCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        uint64_t wid = 0;
        if (listTabsWindowId.empty())
        {
            wid = GetFirstWindowId(server.get());
            if (wid == 0)
            {
                // 0 is the server's "no filter" sentinel, not a real window id;
                // bail rather than silently listing tabs for ALL windows.
                fprintf(stderr, "[wtcli] Could not resolve a window (no windows or ListWindows failed)\n");
                exitCode = 1;
                return;
            }
        }
        else if (!TryParseU64(listTabsWindowId, wid))
        {
            fprintf(stderr, "[wtcli] Invalid --window-id: %s\n", listTabsWindowId.c_str());
            exitCode = 1;
            return;
        }
        Json::Value tabs;
        auto hr = CallJson([&](BSTR* j) { return server->ListTabs(wid, j); }, tabs);
        if (FAILED(hr)) { fprintf(stderr, "ListTabs failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
        {
            Json::Value arr(Json::objectValue);
            arr["tabs"] = tabs;
            PrintJson(arr);
        }
        else
        {
            FormatTabsHuman(tabs);
        }
    });

    // ── list-panes ──
    std::string listPanesTabId, listPanesWindowId;
    auto* listPanesCmd = app.add_subcommand("list-panes", "List panes in a tab")->alias("lsp");
    listPanesCmd->add_option("-t,--tab-id", listPanesTabId, "Tab ID");
    listPanesCmd->add_option("-w,--window-id", listPanesWindowId, "Window ID");
    listPanesCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        uint64_t wid = 0;
        if (!listPanesWindowId.empty() && !TryParseU64(listPanesWindowId, wid))
        {
            fprintf(stderr, "[wtcli] Invalid --window-id: %s\n", listPanesWindowId.c_str());
            exitCode = 1;
            return;
        }
        uint32_t tid = UINT32_MAX;
        if (!listPanesTabId.empty())
        {
            uint64_t t = 0;
            if (!TryParseU64(listPanesTabId, t) || t > UINT32_MAX)
            {
                fprintf(stderr, "[wtcli] Invalid --tab-id: %s\n", listPanesTabId.c_str());
                exitCode = 1;
                return;
            }
            tid = static_cast<uint32_t>(t);
        }
        if (tid == UINT32_MAX)
        {
            if (wid == 0)
            {
                wid = GetFirstWindowId(server.get());
                if (wid == 0)
                {
                    fprintf(stderr, "[wtcli] Could not resolve a window (no windows or ListWindows failed)\n");
                    exitCode = 1;
                    return;
                }
            }
            tid = GetFirstTabId(server.get(), wid);
            if (tid == UINT32_MAX)
            {
                fprintf(stderr, "[wtcli] Could not resolve a tab (no tabs or ListTabs failed)\n");
                exitCode = 1;
                return;
            }
        }
        Json::Value panes;
        auto hr = CallJson([&](BSTR* j) { return server->ListPanes(wid, tid, j); }, panes);
        if (FAILED(hr)) { fprintf(stderr, "ListPanes failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
        {
            Json::Value arr(Json::objectValue);
            arr["panes"] = panes;
            PrintJson(arr);
        }
        else
        {
            FormatPanesHuman(panes);
        }
    });

    // ── active-pane ──
    auto* activePaneCmd = app.add_subcommand("active-pane", "Show the currently active pane");
    activePaneCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        Json::Value info;
        auto hr = CallJson([&](BSTR* j) { return server->GetActivePane(j); }, info);
        if (FAILED(hr)) { fprintf(stderr, "GetActivePane failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
            PrintJson(info);
        else
            FormatActivePaneHuman(info);
    });

    // ── capture-pane ──
    std::string capturePaneTarget;
    int captureMaxLines = 200;
    bool captureLastPrompt = false;
    auto* capturePaneCmd = app.add_subcommand("capture-pane", "Capture pane output")->alias("capturep");
    capturePaneCmd->add_option("-t,--target", capturePaneTarget, "Session ID (GUID)");
    capturePaneCmd->add_option("-l,--max-lines", captureMaxLines, "Max lines");
    capturePaneCmd->add_flag("--last-prompt", captureLastPrompt,
        "Only return the most recent completed shell prompt (command + output, requires OSC 133 shell integration)");
    capturePaneCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        auto sessionId = ResolveSessionId(server.get(), capturePaneTarget);
        wil::unique_bstr src{ Bstr(captureLastPrompt ? "last_prompt" : "scrollback") };
        Json::Value output;
        auto hr = CallJson([&](BSTR* j) { return server->ReadPaneOutput(sessionId, src.get(), captureMaxLines, j); }, output);
        if (FAILED(hr)) { fprintf(stderr, "ReadPaneOutput failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
            PrintJson(output);
        else
            printf("%s\n", output["content"].asString().c_str());
    });

    // ── pane-status ──
    std::string paneStatusTarget;
    auto* paneStatusCmd = app.add_subcommand("pane-status", "Show pane process status");
    paneStatusCmd->add_option("-t,--target", paneStatusTarget, "Session ID (GUID)");
    paneStatusCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        auto sessionId = ResolveSessionId(server.get(), paneStatusTarget);
        Json::Value status;
        auto hr = CallJson([&](BSTR* j) { return server->GetProcessStatus(sessionId, j); }, status);
        if (FAILED(hr)) { fprintf(stderr, "GetProcessStatus failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
            PrintJson(status);
        else
            FormatPaneStatusHuman(status);
    });

    // ── new-tab ──
    std::string newTabCommand, newTabTitle, newTabCwd, newTabProfile;
    auto* newTabCmd = app.add_subcommand("new-tab", "Create a new tab")->alias("neww");
    newTabCmd->add_option("-c,--command", newTabCommand, "Command to run");
    newTabCmd->add_option("-n,--title", newTabTitle, "Tab title");
    newTabCmd->add_option("-d,--cwd", newTabCwd, "Starting directory");
    newTabCmd->add_option("-p,--profile", newTabProfile, "Profile");
    newTabCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        wil::unique_bstr profile{ Bstr(newTabProfile) }, command{ Bstr(newTabCommand) }, title{ Bstr(newTabTitle) }, cwd{ Bstr(newTabCwd) };
        Json::Value result;
        auto hr = CallJson([&](BSTR* j) {
            return server->CreateTab(0, profile.get(), command.get(), title.get(), cwd.get(), false, true, j);
        }, result);
        if (FAILED(hr)) { fprintf(stderr, "CreateTab failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
            PrintJson(result);
        else
            FormatCreatedTabHuman(result);
    });

    // ── split-pane ──
    std::string splitPaneTarget, splitPaneCommand, splitPaneDirection, splitPaneProfile;
    bool splitHorizontal = false, splitVertical = false;
    double splitSize = 0.5;
    auto* splitPaneCmd = app.add_subcommand("split-pane", "Split a pane")->alias("splitw");
    splitPaneCmd->add_option("-t,--target", splitPaneTarget, "Session ID (GUID)");
    splitPaneCmd->add_option("-d,--direction", splitPaneDirection, "Split direction: right|left|up|down|auto");
    splitPaneCmd->add_flag("-H,--horizontal", splitHorizontal, "Split horizontally (legacy alias for --direction down)");
    splitPaneCmd->add_flag("-v,--vertical", splitVertical, "Split vertically (legacy alias for --direction right)");
    splitPaneCmd->add_option("-s,--size", splitSize, "Size fraction");
    splitPaneCmd->add_option("-c,--command", splitPaneCommand, "Command to run");
    splitPaneCmd->add_option("-p,--profile", splitPaneProfile, "Profile");
    splitPaneCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        auto sessionId = ResolveSessionId(server.get(), splitPaneTarget);
        std::string dir;
        if (!splitPaneDirection.empty())
            dir = splitPaneDirection;
        else if (splitHorizontal)
            dir = "down";
        else if (splitVertical)
            dir = "right";
        else
            dir = "automatic";
        wil::unique_bstr dirB{ Bstr(dir) }, profile{ Bstr(splitPaneProfile) }, command{ Bstr(splitPaneCommand) };
        Json::Value result;
        auto hr = CallJson([&](BSTR* j) {
            return server->SplitPane(sessionId, dirB.get(), static_cast<float>(splitSize), profile.get(), command.get(), true, j);
        }, result);
        if (FAILED(hr)) { fprintf(stderr, "SplitPane failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
            PrintJson(result);
        else
            FormatCreatedPaneHuman(result);
    });

    // ── kill-pane ──
    std::string killPaneTarget;
    auto* killPaneCmd = app.add_subcommand("kill-pane", "Close a pane")->alias("killp");
    killPaneCmd->add_option("-t,--target", killPaneTarget, "Session ID (GUID)");
    killPaneCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        auto sessionId = ResolveSessionId(server.get(), killPaneTarget);
        auto hr = server->ClosePane(sessionId);
        if (FAILED(hr)) { fprintf(stderr, "ClosePane failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
        {
            Json::Value v;
            v["ok"] = true;
            v["session_id"] = GuidToString(sessionId);
            PrintJson(v);
        }
        else
        {
            printf("Session %s closed.\n", GuidToString(sessionId).c_str());
        }
    });

    // ── send-keys ──
    std::string sendKeysTarget;
    std::vector<std::string> sendKeysArgs;
    bool sendKeysRaw = false;
    auto* sendKeysCmd = app.add_subcommand("send-keys", "Send keys to a pane")->alias("send");
    sendKeysCmd->add_option("-t,--target", sendKeysTarget, "Session ID (GUID)");
    sendKeysCmd->add_flag("--raw", sendKeysRaw,
                          "Treat the payload as literal UTF-8 text — skip tmux-style "
                          "token translation (Enter/Tab/Escape/BSpace/C-x). Use this when "
                          "forwarding arbitrary agent-supplied text.");
    sendKeysCmd->add_option("keys", sendKeysArgs, "Keys to send")->required();
    sendKeysCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        auto sessionId = ResolveSessionId(server.get(), sendKeysTarget);
        auto text = sendKeysRaw
            ? wtcli::JoinAsUtf16(sendKeysArgs)
            : wtcli::TranslateKeys(sendKeysArgs);
        wil::unique_bstr textB{ SysAllocString(text.c_str()) };
        auto hr = server->SendInput(sessionId, textB.get());
        if (FAILED(hr)) { fprintf(stderr, "SendInput failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
        {
            Json::Value v;
            v["ok"] = true;
            v["session_id"] = GuidToString(sessionId);
            PrintJson(v);
        }
    });

    // ── focus-pane ──
    std::string focusPaneTarget;
    auto* focusPaneCmd = app.add_subcommand("focus-pane", "Switch focus to a pane")->alias("focusp");
    focusPaneCmd->add_option("-t,--target", focusPaneTarget, "Session ID (GUID)");
    focusPaneCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        auto sessionId = ResolveSessionId(server.get(), focusPaneTarget);
        auto hr = server->FocusPane(sessionId);
        if (FAILED(hr)) { fprintf(stderr, "FocusPane failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
        {
            Json::Value v;
            v["ok"] = true;
            v["session_id"] = GuidToString(sessionId);
            PrintJson(v);
        }
        else
        {
            printf("Focused pane %s.\n", GuidToString(sessionId).c_str());
        }
    });

    // ── test-pipe ──
    auto* testPipeCmd = app.add_subcommand("test-pipe", "Test connection to Windows Terminal");
    testPipeCmd->callback([&]() {
        printf("Connecting to Windows Terminal...\n");
        auto server = connect();
        if (!server) { fprintf(stderr, "Connection failed.\n"); return; }
        printf(skipAuthenticate ? "Connected without compatibility handshake!\n\n" : "Connected and authenticated!\n\n");

        Json::Value windows;
        if (SUCCEEDED(CallJson([&](BSTR* j) { return server->ListWindows(j); }, windows)))
        {
            Json::Value arr(Json::objectValue);
            arr["windows"] = windows;
            printf("list_windows:\n");
            PrintJson(arr);
        }
        printf("\n");

        Json::Value caps;
        if (SUCCEEDED(CallJson([&](BSTR* j) { return server->GetCapabilities(j); }, caps)))
        {
            printf("get_capabilities:\n");
            PrintJson(caps);
        }
    });

    // ── info ──
    auto* infoCmd = app.add_subcommand("info", "Show connection info");
    infoCmd->callback([&]() {
        wchar_t clsid[128]{};
        auto hasClsid = GetEnvironmentVariableW(L"WT_COM_CLSID", clsid, ARRAYSIZE(clsid)) > 0;

        std::string version;
        auto server = ConnectToTerminal(nullptr, &version, skipAuthenticate);

        Json::Value methods(Json::arrayValue);
        if (server)
        {
            CallJson([&](BSTR* j) { return server->GetCapabilities(j); }, methods);
        }

        if (jsonMode)
        {
            Json::Value v;
            if (hasClsid)
                v["com_clsid"] = winrt::to_string(winrt::hstring{ clsid });
            v["connected"] = (server != nullptr);
            if (!version.empty())
                v["protocol_version"] = version;
            v["methods"] = methods.isArray() ? methods : Json::Value(Json::arrayValue);
            PrintJson(v);
        }
        else
        {
            printf("Windows Terminal Protocol Info\n");
            printf("========================================\n");
            if (hasClsid)
                printf("  COM CLSID:  %ls\n", clsid);
            else
                printf("  COM CLSID:  (not set)\n");
            printf("\n");
            if (!server)
            {
                printf("  Connection: FAILED\n");
            }
            else
            {
                printf("  Connection: OK\n");
                if (!version.empty())
                    printf("  Protocol:   %s\n", version.c_str());
                printf("\n");
                if (methods.isArray() && methods.size() > 0)
                {
                    printf("  Methods:    %u supported\n", methods.size());
                    for (const auto& m : methods)
                        printf("              - %s\n", m.asString().c_str());
                }
            }
        }

        if (!server)
            exitCode = 1;
    });

    // ── wait-for ──
    std::string waitForTarget;
    int waitInterval = 500;
    int waitTimeout = 0;
    auto* waitForCmd = app.add_subcommand("wait-for", "Wait for a pane to exit");
    waitForCmd->add_option("-t,--target", waitForTarget, "Session ID (GUID)")->required();
    waitForCmd->add_option("--interval", waitInterval, "Poll interval (ms)");
    waitForCmd->add_option("--timeout", waitTimeout, "Timeout (seconds, 0=forever)");
    waitForCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        auto sessionId = ResolveSessionId(server.get(), waitForTarget);
        auto start = std::chrono::steady_clock::now();

        while (true)
        {
            Json::Value status;
            auto hr = CallJson([&](BSTR* j) { return server->GetProcessStatus(sessionId, j); }, status);
            if (FAILED(hr))
            {
                fprintf(stderr, "GetProcessStatus failed: 0x%08X\n", static_cast<uint32_t>(hr));
                exitCode = 1;
                return;
            }
            if (status["state"].asString() == "exited")
            {
                if (jsonMode)
                {
                    Json::Value v;
                    v["state"] = "exited";
                    if (status.isMember("exit_code"))
                        v["exit_code"] = status["exit_code"].asInt();
                    PrintJson(v);
                }
                else
                {
                    printf("Process exited");
                    if (status.isMember("has_exit_code") ? status["has_exit_code"].asBool() : status.isMember("exit_code"))
                        printf(" (code %d)", status["exit_code"].asInt());
                    printf("\n");
                }
                return;
            }

            if (waitTimeout > 0)
            {
                auto elapsed = std::chrono::duration_cast<std::chrono::seconds>(
                                   std::chrono::steady_clock::now() - start)
                                   .count();
                if (elapsed >= waitTimeout)
                {
                    fprintf(stderr, "Timeout waiting for pane %s\n", waitForTarget.c_str());
                    exitCode = 1;
                    return;
                }
            }
            std::this_thread::sleep_for(std::chrono::milliseconds(waitInterval));
        }
    });

    // ── set-env ──
    std::string setEnvShell = "powershell";
    auto* setEnvCmd = app.add_subcommand("set-env", "Print env setup commands")->alias("setenv");
    setEnvCmd->add_option("-s,--shell", setEnvShell, "Shell: powershell, bash, cmd");
    setEnvCmd->callback([&]() {
        wchar_t clsid[128]{};
        GetEnvironmentVariableW(L"WT_COM_CLSID", clsid, ARRAYSIZE(clsid));
        auto cl = winrt::to_string(winrt::hstring{ clsid });

        if (setEnvShell == "powershell" || setEnvShell == "pwsh")
        {
            if (!cl.empty()) printf("$env:WT_COM_CLSID = '%s'\n", cl.c_str());
        }
        else if (setEnvShell == "bash" || setEnvShell == "sh" || setEnvShell == "zsh")
        {
            if (!cl.empty()) printf("export WT_COM_CLSID='%s'\n", cl.c_str());
        }
        else if (setEnvShell == "cmd")
        {
            if (!cl.empty()) printf("set WT_COM_CLSID=%s\n", cl.c_str());
        }
    });

    // ── publish ──
    // Low-level "pass this JSON through to SendEvent verbatim" escape hatch.
    std::string publishJson;
    auto* publishCmd = app.add_subcommand("publish", "Forward raw JSON to SendEvent");
    publishCmd->add_option("json", publishJson, "Full event JSON (e.g. {\"method\":\"autofix_state\",\"params\":{...}})")->required();
    publishCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        wil::unique_bstr evt{ Bstr(publishJson) };
        auto hr = server->SendEvent(evt.get());
        if (FAILED(hr)) { fprintf(stderr, "publish failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; }
    });

    // ── send-event ──
    std::string sendEventType, sendEventJson, sendEventPaneTarget;
    auto* sendEventCmd = app.add_subcommand("send-event", "Publish an event to all listeners")->alias("se");
    sendEventCmd->add_option("-p,--pane", sendEventPaneTarget, "Source session ID (GUID)");
    sendEventCmd->add_option("-e,--event", sendEventType, "Event type (e.g. agent.task.started)")->required();
    sendEventCmd->add_option("json", sendEventJson, "Event params as JSON object");
    sendEventCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        std::string resolvedSessionId;
        if (!sendEventPaneTarget.empty())
        {
            resolvedSessionId = sendEventPaneTarget;
        }
        else
        {
            // Fall back to the active pane as the event source. If there is no
            // active pane, bail rather than sending with an all-zero GUID,
            // which would silently misroute the event.
            const auto activeSid = ResolveSessionId(server.get(), "");
            if (IsEqualGUID(activeSid, GUID{}))
            {
                fprintf(stderr, "[wtcli] send-event: no --pane given and no active pane to use as the event source.\n");
                exitCode = 1;
                return;
            }
            resolvedSessionId = GuidToString(activeSid);
        }
        Json::Value evt;
        if (!wtcli::BuildSendEventJson(sendEventType, sendEventJson, resolvedSessionId, evt))
        {
            fprintf(stderr, "Invalid JSON for --json: value must be a JSON object (e.g. '{\"key\":\"val\"}')\n");
            exitCode = 1;
            return;
        }
        Json::StreamWriterBuilder wb;
        wb["indentation"] = "";
        wil::unique_bstr evtB{ Bstr(Json::writeString(wb, evt)) };
        auto hr = server->SendEvent(evtB.get());
        if (FAILED(hr)) { fprintf(stderr, "SendEvent failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; }
    });

    // ── listen ──
    std::string listenTarget;
    std::string listenEventFilter;
    auto* listenCmd = app.add_subcommand("listen", "Stream real-time events from Windows Terminal");
    listenCmd->add_option("-t,--target", listenTarget, "Filter by session ID (GUID)");
    listenCmd->add_option("--event", listenEventFilter, "Filter by event type (supports trailing wildcard, e.g. agent.*)");
    listenCmd->callback([&]() {
        auto server = connect();
        if (!server) { exitCode = 1; return; }

        static HANDLE s_stopEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        if (!s_stopEvent)
        {
            fprintf(stderr, "[wtcli] listen: failed to create stop event (0x%08X)\n", GetLastError());
            exitCode = 1;
            return;
        }
        SetConsoleCtrlHandler([](DWORD) -> BOOL {
            SetEvent(s_stopEvent);
            return TRUE;
        }, TRUE);

        if (!jsonMode)
            fprintf(stderr, "Listening for events... (Ctrl-C to stop)\n");

        // EventSink is born with _ref == 1, so attach() (adopt, no AddRef) hands
        // that reference to the com_ptr. RAII then Releases on every exit path --
        // exception-safe and robust against future early-returns, no manual
        // Release to forget.
        winrt::com_ptr<ITerminalProtocolEventSink> sink;
        sink.attach(new EventSink([&](const std::string& eventUtf8) {
            if (!wtcli::MatchesEventFilter(eventUtf8, listenTarget, listenEventFilter))
                return;
            printf("%s\n", eventUtf8.c_str());
            fflush(stdout);
        }));

        auto hr = server->Subscribe(sink.get());
        if (FAILED(hr))
        {
            fprintf(stderr, "Subscribe failed: 0x%08X\n", static_cast<uint32_t>(hr));
            exitCode = 1;
            return;
        }

        WaitForSingleObject(s_stopEvent, INFINITE);
        server->Unsubscribe();
        // s_stopEvent is intentionally NOT closed: it is static and still
        // referenced by the registered Ctrl-C handler (a non-capturing lambda
        // that can only reach it via the static), so closing it would leave the
        // handler pointing at an invalid handle. It is reclaimed at process exit.
    });

    // ── Default (no subcommand) ──
    app.callback([&]() {
        if (app.get_subcommands().empty())
        {
            printf("wtcli - Windows Terminal CLI\n\n");
            printf("Usage: wtcli [--json] <subcommand>\n\n");
            printf("Run 'wtcli --help' for available subcommands.\n");
        }
    });

    try
    {
        app.parse(__argc, __argv);
    }
    catch (const CLI::ParseError& e)
    {
        return app.exit(e);
    }

    return exitCode;
}
