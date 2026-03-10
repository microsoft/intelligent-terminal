// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <json/json.h>
#include <functional>

class WindowEmperor;
class AppHost;

// Handles dispatching and executing protocol requests.
// Each method handler receives the parsed params and returns a result or error.
//
// Thread safety: The Handle* methods may be called from any thread (pipe I/O threads).
// They are responsible for marshaling to the UI thread when needed.
class ProtocolRequestHandler
{
public:
    explicit ProtocolRequestHandler(WindowEmperor& emperor);

    // Process a fully parsed request. Returns the response JSON.
    // Called from the pipe I/O thread.
    Json::Value HandleRequest(const Json::Value& request, bool& isAuthenticated);

    // Set the auth token for validation.
    void SetAuthToken(const std::string& token);

private:
    // Method handlers - each returns a Json::Value result or throws
    Json::Value _handleAuthenticate(const Json::Value& params, bool& isAuthenticated);
    Json::Value _handleGetCapabilities(const Json::Value& params);

    // Phase 2: Query operations
    Json::Value _handleGetActivePane(const Json::Value& params);
    Json::Value _handleListWindows(const Json::Value& params);
    Json::Value _handleListTabs(const Json::Value& params);
    Json::Value _handleListPanes(const Json::Value& params);
    Json::Value _handleReadPaneOutput(const Json::Value& params);
    Json::Value _handleGetProcessStatus(const Json::Value& params);
    Json::Value _handleGetSessionVariable(const Json::Value& params);
    Json::Value _handleGetSettings(const Json::Value& params);

    // Phase 3: Mutation operations
    Json::Value _handleCreateTab(const Json::Value& params);
    Json::Value _handleSplitPane(const Json::Value& params);
    Json::Value _handleClosePane(const Json::Value& params);
    Json::Value _handleSendInput(const Json::Value& params);
    Json::Value _handleSetSessionVariable(const Json::Value& params);
    Json::Value _handleSetSettings(const Json::Value& params);

    // Helper to build error responses
    static Json::Value _makeError(const std::string& code, const std::string& message);

    // Helper to build success responses
    static Json::Value _makeResponse(const std::string& id, const Json::Value& result);
    static Json::Value _makeErrorResponse(const std::string& id, const std::string& code, const std::string& message);

    // Find pane globally across all windows/tabs. Returns window ID, tab, and pane.
    struct PaneLookupResult
    {
        uint64_t windowId = 0;
        AppHost* appHost = nullptr;
        uint32_t tabIndex = 0;
        uint32_t paneId = 0;
        bool found = false;
    };
    PaneLookupResult _findPaneGlobally(uint32_t paneId);

    // Per-action confirmation (Phase 4)
    enum class RiskLevel
    {
        Read, // Auto-approve by default
        Create, // Confirm by default
        Input // Always confirm by default
    };
    static RiskLevel _getRiskLevel(const std::string& method);
    bool _checkConfirmation(const std::string& method, const Json::Value& params);

    WindowEmperor& _emperor;
    std::string _authToken;

    // Confirmation policy: auto-approve all for development.
    // Set to false to require user confirmation for create/input operations.
    bool _autoApproveAll = true;

    // Supported methods list for get_capabilities
    static const std::vector<std::string> _supportedMethods;
};
