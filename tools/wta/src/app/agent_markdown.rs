// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! Response-owned Markdown presentation. Sources are immutable until the caller invalidates
//! their identity; reading a snapshot never parses, schedules work, or compares source text.

use std::collections::HashMap;
use std::sync::{mpsc, Arc};
use std::time::Instant;

use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use tui_markdown::{from_str_with_options, AlertKind, Options, StyleSheet};
use unicode_width::UnicodeWidthStr;

use crate::theme;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct WorkStats {
    pub submitted_jobs: u64,
    pub submitted_source_bytes: u64,
    pub completed_batch_calls: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ResponseKey {
    Current,
    Completed(usize),
}

pub(crate) struct AgentMarkdownCache {
    entries: HashMap<ResponseKey, Response>,
    generation: u64,
    in_flight: Option<(ResponseKey, u64)>,
    worker: Option<Worker>,
    worker_failed: bool,
    enabled: bool,
    #[cfg(test)]
    stats: WorkStats,
}

impl Default for AgentMarkdownCache {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            generation: 0,
            in_flight: None,
            worker: None,
            worker_failed: false,
            enabled: true,
            #[cfg(test)]
            stats: WorkStats::default(),
        }
    }
}

struct Response {
    sources: Arc<[Arc<str>]>,
    width: usize,
    generation: u64,
    queued: bool,
    seen: bool,
    requested: bool,
    snapshot: Snapshot,
}

enum Snapshot {
    Pending,
    Ready(Vec<Vec<Line<'static>>>),
    Published(Vec<Vec<Line<'static>>>),
    Unavailable,
}

struct BatchJob {
    sources: Arc<[Arc<str>]>,
    width: usize,
    generation: u64,
}

struct BatchResult {
    generation: u64,
    completed_batch_calls: u64,
    rows: Result<Vec<Vec<Line<'static>>>, ProjectionLimit>,
}

#[derive(Debug)]
enum ProjectionLimit {
    GraphemeTooWide,
    CoordinateRange,
    OverwideRow,
}

struct Worker {
    requests: mpsc::Sender<BatchJob>,
    results: mpsc::Receiver<BatchResult>,
}

impl Worker {
    fn start() -> std::io::Result<Self> {
        let (requests, request_rx) = mpsc::channel::<BatchJob>();
        let (result_tx, results) = mpsc::channel();
        let dispatcher = tracing::dispatcher::get_default(Clone::clone);
        // Closing the channels ends the detached worker after its current immutable job;
        // UI teardown never waits for a parse, and no second job is sent while one is in flight.
        std::thread::Builder::new()
            .name("agent-markdown".into())
            .spawn(move || {
                tracing::dispatcher::with_default(&dispatcher, || {
                    while let Ok(job) = request_rx.recv() {
                        if result_tx.send(render_batch(job)).is_err() {
                            break;
                        }
                    }
                });
            })?;
        Ok(Self { requests, results })
    }
}

impl AgentMarkdownCache {
    pub(crate) fn set_enabled(&mut self, enabled: bool, allow_publication: bool) -> bool {
        let changed = self.enabled != enabled;
        self.enabled = enabled;
        if changed && enabled {
            self.worker_failed = false;
        }
        // Stop new work immediately, but retain the selected display until the
        // same guard that publishes Markdown allows switching back to raw.
        if !enabled && allow_publication {
            let had_entries = !self.entries.is_empty();
            self.entries.clear();
            changed || had_entries
        } else {
            false
        }
    }

    pub(crate) fn request<'a>(
        &mut self,
        turn: usize,
        width: usize,
        sources: impl Iterator<Item = &'a str>,
    ) {
        self.request_for(ResponseKey::Completed(turn), width, sources);
    }

    pub(crate) fn request_current<'a>(
        &mut self,
        width: usize,
        sources: impl Iterator<Item = &'a str>,
    ) {
        self.request_for(ResponseKey::Current, width, sources);
    }

    fn request_for<'a>(
        &mut self,
        key: ResponseKey,
        width: usize,
        sources: impl Iterator<Item = &'a str>,
    ) {
        if !self.enabled {
            if let Some(response) = self.entries.get_mut(&key) {
                response.seen = true;
            }
            return;
        }
        if width == 0 {
            return;
        }
        if let Some(response) = self.entries.get_mut(&key) {
            response.seen = true;
            response.requested = true;
            if response.width == width {
                self.dispatch();
                return;
            }
            self.generation = self.generation.wrapping_add(1);
            response.width = width;
            response.generation = self.generation;
            response.queued = true;
            response.snapshot = Snapshot::Pending;
        } else {
            let sources: Vec<Arc<str>> = sources.map(Arc::from).collect();
            if sources.is_empty() {
                return;
            }
            self.generation = self.generation.wrapping_add(1);
            self.entries.insert(
                key,
                Response {
                    sources: sources.into(),
                    width,
                    generation: self.generation,
                    queued: true,
                    seen: true,
                    requested: true,
                    snapshot: Snapshot::Pending,
                },
            );
        }
        self.dispatch();
    }

    pub(crate) fn rows(
        &self,
        turn: usize,
        segment: usize,
        width: usize,
    ) -> Option<&[Line<'static>]> {
        self.rows_for(ResponseKey::Completed(turn), segment, width)
    }

    pub(crate) fn current_rows(&self, segment: usize, width: usize) -> Option<&[Line<'static>]> {
        self.rows_for(ResponseKey::Current, segment, width)
    }

    fn rows_for(&self, key: ResponseKey, segment: usize, width: usize) -> Option<&[Line<'static>]> {
        let response = self
            .entries
            .get(&key)
            .filter(|entry| entry.width == width)?;
        match &response.snapshot {
            Snapshot::Published(segments) => segments.get(segment).map(Vec::as_slice),
            _ => None,
        }
    }

    pub(crate) fn poll(&mut self, allow_publication: bool) -> Vec<usize> {
        if !self.enabled {
            return Vec::new();
        }
        let received = self.worker.as_ref().map(|worker| worker.results.try_recv());
        match received {
            Some(Ok(result)) => {
                if !self.accept_result(result) {
                    return Vec::new();
                }
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.disable_worker("result_disconnected");
            }
            _ => {}
        }
        let mut changed = Vec::new();
        if allow_publication {
            for (&key, response) in &mut self.entries {
                if matches!(response.snapshot, Snapshot::Ready(_)) {
                    if let Snapshot::Ready(rows) =
                        std::mem::replace(&mut response.snapshot, Snapshot::Pending)
                    {
                        response.snapshot = Snapshot::Published(rows);
                        if let ResponseKey::Completed(turn) = key {
                            changed.push(turn);
                        }
                    }
                }
            }
        }
        changed
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.enabled
            && !self.worker_failed
            && (self.in_flight.is_some()
                || self
                    .entries
                    .values()
                    .any(|entry| entry.queued && entry.requested))
    }

    pub(crate) fn begin_frame(&mut self) {
        for response in self.entries.values_mut() {
            response.seen = false;
            response.requested = false;
        }
    }

    pub(crate) fn touch(&mut self, turn: usize) {
        if let Some(response) = self.entries.get_mut(&ResponseKey::Completed(turn)) {
            response.seen = true;
        }
    }

    pub(crate) fn end_frame(&mut self) -> Vec<usize> {
        let mut removed = Vec::new();
        self.entries.retain(|key, response| {
            if !response.seen {
                if let ResponseKey::Completed(turn) = key {
                    removed.push(*turn);
                }
            }
            response.seen
        });
        removed
    }

    pub(crate) fn invalidate(&mut self, turn: usize) {
        self.entries.remove(&ResponseKey::Completed(turn));
    }

    pub(crate) fn invalidate_current(&mut self) {
        self.entries.remove(&ResponseKey::Current);
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        // Keep the in-flight slot and generation: a pre-clear result cannot belong to
        // a new response that reuses the same history index.
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> WorkStats {
        self.stats
    }

    fn accept_result(&mut self, result: BatchResult) -> bool {
        #[cfg(test)]
        {
            self.stats.completed_batch_calls += result.completed_batch_calls;
        }
        let Some((key, generation)) = self.in_flight.take() else {
            self.disable_worker("unexpected_result");
            return false;
        };
        if result.generation != generation {
            self.disable_worker("unexpected_generation");
            return false;
        }
        let response = self
            .entries
            .get_mut(&key)
            .filter(|entry| entry.generation == generation);
        tracing::debug!(
            target: "agent_markdown",
            ?key,
            generation,
            completed_batch_calls = result.completed_batch_calls,
            accepted = response.is_some(),
            "Markdown result received"
        );
        if let Some(response) = response {
            response.snapshot = match result.rows {
                Ok(rows) => Snapshot::Ready(rows),
                Err(_) => Snapshot::Unavailable,
            };
        }
        true
    }

    fn dispatch(&mut self) {
        if self.worker_failed || self.in_flight.is_some() {
            return;
        }
        let next = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.queued && entry.requested)
            .min_by_key(|(_, entry)| entry.generation)
            .map(|(&turn, _)| turn);
        let Some(turn) = next else {
            return;
        };
        if self.worker.is_none() {
            match Worker::start() {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => {
                    tracing::error!(
                        target: "agent_markdown",
                        %error,
                        "Cannot start Markdown rendering; retaining raw display"
                    );
                    self.worker_failed = true;
                    return;
                }
            }
        }
        let Some(response) = self.entries.get_mut(&turn) else {
            return;
        };
        let Some(worker) = self.worker.as_ref() else {
            return;
        };
        let generation = response.generation;
        let job = BatchJob {
            sources: Arc::clone(&response.sources),
            width: response.width,
            generation,
        };
        #[cfg(test)]
        let source_bytes: usize = response.sources.iter().map(|source| source.len()).sum();
        if worker.requests.send(job).is_err() {
            self.disable_worker("request_disconnected");
        } else {
            response.queued = false;
            self.in_flight = Some((turn, generation));
            #[cfg(test)]
            {
                self.stats.submitted_jobs += 1;
                self.stats.submitted_source_bytes += source_bytes as u64;
            }
        }
    }

    fn disable_worker(&mut self, reason: &'static str) {
        tracing::error!(
            target: "agent_markdown",
            reason,
            "Markdown worker stopped; retaining raw display without automatic retries"
        );
        self.worker = None;
        self.in_flight = None;
        self.worker_failed = true;
    }

    #[cfg(test)]
    fn finish_jobs(&mut self) {
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while self.has_pending() {
            self.dispatch();
            let result = self
                .worker
                .as_ref()
                .expect("a submitted job has a worker")
                .results
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("the deterministic renderer must complete");
            assert!(self.accept_result(result));
            self.poll(true);
        }
    }
}

#[derive(Clone, Copy)]
struct PaneStyles;

impl StyleSheet for PaneStyles {
    fn heading(&self, level: u8) -> Style {
        Style::new().add_modifier(match level {
            1 => Modifier::BOLD | Modifier::UNDERLINED,
            2 => Modifier::BOLD,
            3 => Modifier::BOLD | Modifier::ITALIC,
            _ => Modifier::ITALIC,
        })
    }

    fn heading_marker(&self, _level: u8) -> &str {
        ""
    }

    fn code(&self) -> Style {
        Style::new().add_modifier(Modifier::DIM)
    }

    fn link(&self) -> Style {
        Style::new().add_modifier(Modifier::UNDERLINED)
    }

    fn blockquote(&self) -> Style {
        Style::new().add_modifier(Modifier::DIM | Modifier::ITALIC)
    }

    fn heading_meta(&self) -> Style {
        Style::new().add_modifier(Modifier::DIM)
    }

    fn metadata_block(&self) -> Style {
        Style::new().add_modifier(Modifier::DIM)
    }

    fn html(&self) -> Style {
        Style::new().add_modifier(Modifier::DIM)
    }

    fn math_inline(&self) -> Style {
        Style::new().add_modifier(Modifier::ITALIC)
    }

    fn math_display(&self) -> Style {
        Style::new().add_modifier(Modifier::ITALIC)
    }

    fn footnote_ref(&self) -> Style {
        Style::new().add_modifier(Modifier::UNDERLINED)
    }

    fn footnote_def(&self) -> Style {
        Style::new().add_modifier(Modifier::DIM)
    }

    fn definition_term(&self) -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }

    fn definition_description(&self) -> Style {
        Style::new()
    }

    fn alert(&self, _kind: AlertKind) -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }

    fn table_header(&self) -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }

    fn table_cell(&self) -> Style {
        Style::new()
    }

    fn table_border(&self) -> Style {
        Style::new().add_modifier(Modifier::DIM)
    }

    fn list_marker(&self) -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }
}

fn body_width(width: usize) -> u16 {
    let width = width.min(usize::from(u16::MAX)) as u16;
    if width > 2 {
        width - 2
    } else {
        width
    }
}

fn render_batch(job: BatchJob) -> BatchResult {
    let started = Instant::now();
    let options = Options::new(PaneStyles).table_width(body_width(job.width));
    // Upstream's debug spans include parsed text and link destinations. Keep
    // diagnostics at the host boundary without recording response content.
    let parser_dispatch = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let mut completed_batch_calls = 0;
    let rows = job
        .sources
        .iter()
        .map(|source| {
            let text = tracing::dispatcher::with_default(&parser_dispatch, || {
                from_str_with_options(source, &options)
            });
            completed_batch_calls += 1;
            project_text(&text, job.width)
        })
        .collect::<Result<Vec<_>, _>>();
    let source_bytes: usize = job.sources.iter().map(|source| source.len()).sum();
    if let Err(reason) = &rows {
        tracing::warn!(
            target: "agent_markdown",
            ?reason,
            width = job.width,
            source_bytes,
            "Markdown projection exceeds Ratatui layout limits; retaining raw display and source"
        );
    }
    tracing::debug!(
        target: "agent_markdown",
        generation = job.generation,
        segments = job.sources.len(),
        source_bytes,
        width = job.width,
        elapsed_us = started.elapsed().as_micros(),
        rendered = rows.is_ok(),
        completed_batch_calls,
        "Markdown batch finished"
    );
    BatchResult {
        generation: job.generation,
        completed_batch_calls,
        rows,
    }
}

fn project_text(text: &Text<'_>, width: usize) -> Result<Vec<Line<'static>>, ProjectionLimit> {
    let body = body_width(width);
    let mut rows = Vec::new();
    let mut marked = false;
    for line in &text.lines {
        let line = prepare_line(line, text.style, text.alignment);
        for mut line in project_line(line, body)? {
            if line
                .spans
                .iter()
                .any(|span| !span.content.trim().is_empty())
            {
                if !marked {
                    marked = true;
                    if width <= 2 {
                        rows.push(Line::from(Span::styled("●", theme::DOT_AGENT)));
                    } else {
                        line.spans.insert(0, Span::styled("● ", theme::DOT_AGENT));
                    }
                } else if width > 2 {
                    line.spans.insert(0, Span::raw("  "));
                }
            }
            rows.push(line);
        }
    }
    Ok(rows)
}

fn push_symbol(spans: &mut Vec<Span<'static>>, symbol: &str, style: Style) {
    if let Some(last) = spans.last_mut().filter(|span| span.style == style) {
        last.content.to_mut().push_str(symbol);
    } else {
        spans.push(Span::styled(symbol.to_owned(), style));
    }
}

fn prepare_line<'a>(
    line: &'a Line<'_>,
    text_style: Style,
    alignment: Option<Alignment>,
) -> Line<'a> {
    let base = text_style.patch(line.style);
    if line.spans.len() <= 1 {
        return Line {
            style: base,
            alignment: line.alignment.or(alignment),
            spans: line
                .spans
                .iter()
                .map(|span| Span::styled(span.content.as_ref(), span.style))
                .collect(),
        };
    }
    // Use Ratatui's grapheme segmentation across inline style boundaries. The
    // leading source byte determines the style of an indivisible display cell.
    let joined = Span::raw(
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
    );
    let mut spans = Vec::new();
    let mut index = 0;
    let mut end = line.spans[0].content.len();
    for grapheme in joined.styled_graphemes(base) {
        let byte = grapheme.symbol.as_ptr().addr() - joined.content.as_ptr().addr();
        while byte >= end && index + 1 < line.spans.len() {
            index += 1;
            end += line.spans[index].content.len();
        }
        push_symbol(
            &mut spans,
            grapheme.symbol,
            base.patch(line.spans[index].style),
        );
    }
    Line {
        spans,
        style: Style::default(),
        alignment: line.alignment.or(alignment),
    }
}

fn project_line(line: Line<'_>, width: u16) -> Result<Vec<Line<'static>>, ProjectionLimit> {
    if line
        .styled_graphemes(Style::default())
        .any(|grapheme| grapheme.symbol.width() > usize::from(width))
    {
        return Err(ProjectionLimit::GraphemeTooWide);
    }
    let mut paragraph = Paragraph::new(line)
        .style(theme::AGENT_TEXT)
        .wrap(Wrap { trim: false });
    let height = paragraph.line_count(width);
    if height > usize::from(u16::MAX) * 2 {
        return Err(ProjectionLimit::CoordinateRange);
    }
    let mut rows = Vec::with_capacity(height);
    while rows.len() < height {
        let page_height = (height - rows.len()).min(usize::from(u16::MAX)) as u16;
        let area = Rect::new(0, 0, width, page_height);
        let mut buffer = Buffer::filled(area, Cell::new(""));
        paragraph = paragraph.scroll((rows.len() as u16, 0));
        (&paragraph).render(area, &mut buffer);
        for y in 0..page_height {
            let mut spans = Vec::new();
            let start = usize::from(y) * usize::from(width);
            let cells = &buffer.content[start..start + usize::from(width)];
            if let Some(last) = cells.iter().rposition(|cell| !cell.symbol().is_empty()) {
                let mut x = 0;
                while x <= last {
                    let cell = &cells[x];
                    let symbol = if cell.symbol().is_empty() {
                        " "
                    } else {
                        cell.symbol()
                    };
                    push_symbol(&mut spans, symbol, cell.style());
                    x += symbol.width().max(1);
                }
            }
            let row = Line::from(spans);
            if row.width() > usize::from(width) {
                return Err(ProjectionLimit::OverwideRow);
            }
            rows.push(row);
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Write};
    use std::sync::Mutex;

    #[derive(Clone)]
    struct LogWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn agent_markdown_diagnostics_do_not_include_response_content() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::clone(&output);
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .without_time()
            .with_writer(move || LogWriter(Arc::clone(&writer)))
            .finish();
        let marker = "PRIVATE_MARKDOWN_DIAGNOSTIC_SENTINEL";
        let source = format!("[visible](https://example.invalid/{marker})");
        tracing::subscriber::with_default(subscriber, || {
            let mut cache = AgentMarkdownCache::default();
            cache.request(0, 90, std::iter::once(source.as_str()));
            cache.finish_jobs();
            assert!(cache.rows(0, 0, 90).is_some());
        });
        let log = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(log.contains("Markdown batch finished"), "{log}");
        assert!(
            !log.contains(marker),
            "Markdown source reached diagnostics:\n{log}"
        );
    }

    #[test]
    fn agent_markdown_projection_matches_fresh_package_and_ratatui_output() {
        let sources = [
            "# Heading\r\n\r\n- [x] **Done**\r\n- [ ] pending\r\n",
            "> quote with *emphasis*\n\n[docs](https://example.invalid)\n\n`code`",
            "```rust\nlet value = 1;\n```\n\n```text\nunfinished",
            "| Name | Value |\n| :--- | ---: |\n| wide | **bold** |\n| short | 42 |",
            "Unicode: 中文 e\u{301} 👩‍💻 🌍\n\n**unfinished",
        ];
        for width in [8, 24, 90] {
            for source in sources {
                let body = body_width(width);
                let options = Options::new(PaneStyles).table_width(body);
                let canonical = from_str_with_options(source, &options);
                let paragraph = Paragraph::new(canonical)
                    .style(theme::AGENT_TEXT)
                    .wrap(Wrap { trim: false });
                let height = paragraph.line_count(body);
                let area = Rect::new(0, 0, body, height as u16);
                let mut expected = Buffer::empty(area);
                paragraph.render(area, &mut expected);
                let result = render_batch(BatchJob {
                    sources: vec![Arc::from(source)].into(),
                    width,
                    generation: 1,
                });
                // Ratatui can place a wide grapheme in the final cell of a
                // narrow wrapped row. Reject that lossy layout rather than
                // substituting a second wrapping algorithm.
                if expected.content.chunks(usize::from(body)).any(|row| {
                    row.iter()
                        .enumerate()
                        .any(|(x, cell)| x + cell.symbol().width() > usize::from(body))
                }) {
                    assert!(matches!(result.rows, Err(ProjectionLimit::OverwideRow)));
                    continue;
                }
                let mut segments = result.rows.unwrap_or_else(|reason| {
                    panic!("width={width}, source={source:?}, reason={reason:?}")
                });
                let mut rows = segments.remove(0);
                assert_eq!(result.completed_batch_calls, 1);
                for row in &mut rows {
                    assert!(row.width() <= width, "{row:?}");
                    if row.spans.iter().any(|span| !span.content.trim().is_empty()) {
                        let marker = row.spans.remove(0);
                        assert!(matches!(marker.content.as_ref(), "● " | "  "));
                    }
                }

                assert_eq!(rows.len(), height, "width={width}, source={source:?}");
                let mut actual = Buffer::empty(area);
                Paragraph::new(rows)
                    .style(theme::AGENT_TEXT)
                    .render(area, &mut actual);
                assert!(actual.content.iter().all(|cell| {
                    cell.fg == ratatui::style::Color::Reset
                        && cell.bg == ratatui::style::Color::Reset
                }));
                assert_eq!(actual, expected, "width={width}, source={source:?}");
            }
        }
    }

    #[test]
    fn agent_markdown_rejects_stale_width_and_reuses_compatible_sources() {
        let source = "**A reply that wraps at the narrower width**";
        let mut cache = AgentMarkdownCache::default();
        cache.request(0, 90, std::iter::once(source));
        cache.request(0, 12, std::iter::empty());
        cache.finish_jobs();
        assert!(cache.rows(0, 0, 90).is_none());
        let rows = cache.rows(0, 0, 12).unwrap();
        assert!(rows.len() > 1);
        assert!(rows.iter().all(|row| row.width() <= 12));
        assert_eq!(cache.stats().completed_batch_calls, 2);
        assert_eq!(
            cache.entries[&ResponseKey::Completed(0)].sources[0].as_ref(),
            source
        );

        let before = cache.stats();
        cache.request(
            0,
            12,
            std::iter::once_with(|| panic!("compatible frames must not read source")),
        );
        cache.finish_jobs();
        assert_eq!(cache.stats(), before);
        cache.begin_frame();
        assert_eq!(cache.end_frame(), [0]);
        assert!(cache.rows(0, 0, 12).is_none());
        cache.request(0, 12, std::iter::once(source));
        cache.finish_jobs();
        assert_eq!(
            cache.stats().completed_batch_calls,
            before.completed_batch_calls + 1
        );

        assert!(!cache.set_enabled(false, false));
        cache.begin_frame();
        cache.request(0, 12, std::iter::empty());
        assert!(cache.end_frame().is_empty());
        assert!(
            cache.rows(0, 0, 12).is_some(),
            "selected display remains available"
        );
        assert!(!cache.has_pending());
        assert!(cache.set_enabled(false, true));
        assert!(
            cache.entries.is_empty(),
            "disabled unselected mode releases derived state"
        );
    }

    #[test]
    fn agent_markdown_unrepresentable_width_keeps_source_without_repeated_work() {
        for (source, width) in [
            ("**中文**", 1),
            ("Unicode: 中文 e\u{301} 👩‍💻 🌍\n\n**unfinished", 8),
        ] {
            let mut cache = AgentMarkdownCache::default();
            cache.request(0, width, std::iter::once(source));
            cache.finish_jobs();
            assert!(cache.rows(0, 0, width).is_none());
            assert_eq!(
                cache.entries[&ResponseKey::Completed(0)].sources[0].as_ref(),
                source
            );
            let before = cache.stats();
            for _ in 0..3 {
                cache.request(0, width, std::iter::once(source));
            }
            cache.finish_jobs();
            assert_eq!(cache.stats(), before);
            cache.request(0, 90, std::iter::empty());
            cache.finish_jobs();
            assert!(cache.rows(0, 0, 90).is_some());
        }
    }
}
