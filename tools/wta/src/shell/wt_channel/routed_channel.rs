// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// RoutedChannel — dispatches requests to one of two underlying WtChannels
// based on method name. The "primary" channel is used for methods it claims
// to support; everything else falls through to the "fallback".
//
// This is the cutover mechanism for migrating individual methods off the
// COM/wtcli transport (CliChannel) onto the inherited-pipe transport
// (PipeChannel). Today only `send_input` is on the primary; future critical
// methods (CreateTab, SplitPane, ClosePane, ...) join the list as they
// migrate.

use std::sync::Arc;

use async_trait::async_trait;

use super::WtChannel;

pub struct RoutedChannel {
    primary: Arc<dyn WtChannel>,
    fallback: Arc<dyn WtChannel>,
    primary_methods: &'static [&'static str],
}

impl RoutedChannel {
    pub fn new(
        primary: Arc<dyn WtChannel>,
        fallback: Arc<dyn WtChannel>,
        primary_methods: &'static [&'static str],
    ) -> Self {
        Self {
            primary,
            fallback,
            primary_methods,
        }
    }
}

#[async_trait]
impl WtChannel for RoutedChannel {
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        if self.primary.is_available() && self.primary_methods.contains(&method) {
            self.primary.request(method, params).await
        } else {
            self.fallback.request(method, params).await
        }
    }

    fn is_available(&self) -> bool {
        self.fallback.is_available() || self.primary.is_available()
    }
}
