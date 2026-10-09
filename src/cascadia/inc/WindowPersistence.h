// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <filesystem>
#include <unordered_set>
#include <vector>
#include <sddl.h>
#include <cppwinrt_utils.h>
#include <til/hash.h>
#include <til/winrt.h>
#include <winrt/TerminalApp.h>
#include <winrt/Microsoft.Terminal.Settings.Model.h>

namespace Microsoft::Terminal::WindowPersistence
{
    namespace Model = winrt::Microsoft::Terminal::Settings::Model;

    struct Snapshot
    {
        Model::WindowLayout Layout{ nullptr };
        winrt::hstring Name;
    };

    using BufferFiles = std::unordered_set<std::wstring, til::transparent_hstring_hash, til::transparent_hstring_equal_to>;

    inline void AppendLayout(const Model::ApplicationState& state, const Snapshot& snapshot)
    {
        if (!snapshot.Layout)
        {
            return;
        }
        if (snapshot.Name.empty())
        {
            state.AppendPersistedWindowLayout(snapshot.Layout);
            return;
        }

        state.SaveWorkspace(snapshot.Name, snapshot.Layout);
        Model::WindowLayout stub;
        stub.TabLayout(winrt::single_threaded_vector<Model::ActionAndArgs>({ Model::ActionAndArgs{ Model::ShortcutAction::OpenWorkspace, Model::OpenWorkspaceArgs{ snapshot.Name } } }));
        state.AppendPersistedWindowLayout(stub);
    }

    inline void PersistLayouts(const Model::ApplicationState& state,
                               const std::vector<Snapshot>& windows,
                               const Snapshot& lastClosedWindow,
                               const bool enabled)
    {
        if (state.PersistedWindowLayouts())
        {
            state.PersistedWindowLayouts(nullptr);
        }
        if (enabled)
        {
            if (windows.empty())
            {
                AppendLayout(state, lastClosedWindow);
            }
            else
            {
                for (const auto& window : windows)
                {
                    AppendLayout(state, window);
                }
            }
        }
        state.Flush();
    }

    inline std::wstring BufferFilename(const winrt::guid& sessionId, const bool elevated)
    {
        return fmt::format(FMT_COMPILE(L"{}{}.txt"), elevated ? L"elevated_" : L"buffer_", sessionId);
    }

    template<typename Panes>
    BufferFiles PersistBuffers(const Panes& panes, const std::filesystem::path& directory, const bool elevated)
    {
        wil::unique_hlocal_security_descriptor sd;
        SECURITY_ATTRIBUTES sa{};
        if (elevated)
        {
            unsigned long cb;
            THROW_IF_WIN32_BOOL_FALSE(ConvertStringSecurityDescriptorToSecurityDescriptorW(
                L"S:(ML;;NRNW;;;HI)", SDDL_REVISION_1, wil::out_param_ptr<PSECURITY_DESCRIPTOR*>(sd), &cb));
            sa.nLength = sizeof(SECURITY_ATTRIBUTES);
            sa.lpSecurityDescriptor = sd.get();
        }

        BufferFiles files;
        for (const auto& pane : panes)
        {
            try
            {
                const auto term = pane.template try_as<winrt::TerminalApp::ITerminalPaneContent>();
                const auto control = term ? term.GetTermControl() : nullptr;
                const auto connection = control ? control.Connection() : nullptr;
                if (!connection || connection.SessionId() == winrt::guid{})
                {
                    continue;
                }
                auto filename = BufferFilename(connection.SessionId(), elevated);
                const auto path = directory / filename;
                wil::unique_hfile file{ CreateFileW(path.c_str(), GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_DELETE, elevated ? &sa : nullptr, OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr) };
                THROW_LAST_ERROR_IF(!file);
                control.PersistTo(reinterpret_cast<int64_t>(file.get()));
                // An uninitialized control leaves its previous buffer untouched.
                LARGE_INTEGER written{};
                THROW_IF_WIN32_BOOL_FALSE(SetFilePointerEx(file.get(), {}, &written, FILE_CURRENT));
                if (written.QuadPart > 0)
                {
                    THROW_IF_WIN32_BOOL_FALSE(SetEndOfFile(file.get()));
                }
                files.emplace(std::move(filename));
            }
            CATCH_LOG()
        }
        return files;
    }

    inline BufferFiles BufferFilesForLayout(const Model::WindowLayout& layout, const bool elevated)
    {
        BufferFiles files;
        if (layout)
        {
            for (const auto& action : layout.TabLayout())
            {
                Model::INewContentArgs contentArgs{ nullptr };
                if (const auto newTab = action.Args().try_as<Model::NewTabArgs>())
                {
                    contentArgs = newTab.ContentArgs();
                }
                else if (const auto split = action.Args().try_as<Model::SplitPaneArgs>())
                {
                    contentArgs = split.ContentArgs();
                }
                if (const auto args = contentArgs.try_as<Model::NewTerminalArgs>(); args && args.SessionId() != winrt::guid{})
                {
                    files.emplace(BufferFilename(args.SessionId(), elevated));
                }
            }
        }
        return files;
    }
}
