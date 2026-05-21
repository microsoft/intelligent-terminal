// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Library target for WTA — exposes functions needed by fuzz targets
// and tests without pulling in the full binary's module tree.
//
// Only the pure-logic functions are re-exported here. Modules with
// runtime dependencies (wt_channel, app, protocol) stay in main.rs.

mod shell_fuzz;

pub use shell_fuzz::{build_wt_commandline, BuildCommandlineError};
