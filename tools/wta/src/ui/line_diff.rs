use std::collections::HashSet;

const MAX_DIFF_CORE_LINES: usize = 256;
const CONTEXT_LINES: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DiffLineKind {
    Context,
    Added,
    Removed,
    Omitted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DiffLine<'a> {
    pub(crate) kind: DiffLineKind,
    pub(crate) text: &'a str,
}

pub(crate) fn preview<'a>(
    old_text: Option<&'a str>,
    new_text: &'a str,
    source_truncated: bool,
    max_lines: usize,
) -> Vec<DiffLine<'a>> {
    let old_lines = old_text.map_or_else(Vec::new, snapshot_lines);
    let new_lines = snapshot_lines(new_text);
    let mut lines = Vec::new();

    if source_truncated {
        push_omitted(&mut lines);
    }

    if old_text.is_none() {
        lines.extend(
            new_lines
                .iter()
                .take(MAX_DIFF_CORE_LINES)
                .map(|text| DiffLine {
                    kind: DiffLineKind::Added,
                    text,
                }),
        );
        if new_lines.len() > MAX_DIFF_CORE_LINES {
            push_omitted(&mut lines);
        }
        return balance_preview(lines, max_lines);
    }

    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }

    let mut suffix = 0;
    while suffix < old_lines.len().saturating_sub(prefix)
        && suffix < new_lines.len().saturating_sub(prefix)
        && old_lines[old_lines.len() - suffix - 1] == new_lines[new_lines.len() - suffix - 1]
    {
        suffix += 1;
    }

    if prefix == old_lines.len() && prefix == new_lines.len() {
        return balance_preview(lines, max_lines);
    }

    if prefix > CONTEXT_LINES {
        push_omitted(&mut lines);
    }
    lines.extend(
        old_lines[prefix.saturating_sub(CONTEXT_LINES)..prefix]
            .iter()
            .map(|text| DiffLine {
                kind: DiffLineKind::Context,
                text,
            }),
    );

    let old_core_end = old_lines.len() - suffix;
    let new_core_end = new_lines.len() - suffix;
    let old_core = &old_lines[prefix..old_core_end];
    let new_core = &new_lines[prefix..new_core_end];
    let core_exceeds_limit =
        old_core.len() > MAX_DIFF_CORE_LINES || new_core.len() > MAX_DIFF_CORE_LINES;
    if core_exceeds_limit && shares_line(old_core, new_core) {
        append_large_shared_core(&mut lines, old_core, new_core);
    } else {
        let old_bounded = &old_core[..old_core.len().min(MAX_DIFF_CORE_LINES)];
        let new_bounded = &new_core[..new_core.len().min(MAX_DIFF_CORE_LINES)];
        append_collapsed_ops(&mut lines, &diff_ops(old_bounded, new_bounded));
        if core_exceeds_limit {
            push_omitted(&mut lines);
        }
    }

    let suffix_context = suffix.min(CONTEXT_LINES);
    lines.extend(
        old_lines[old_core_end..old_core_end + suffix_context]
            .iter()
            .map(|text| DiffLine {
                kind: DiffLineKind::Context,
                text,
            }),
    );
    if suffix > CONTEXT_LINES {
        push_omitted(&mut lines);
    }

    balance_preview(lines, max_lines)
}

fn snapshot_lines(text: &str) -> Vec<&str> {
    let mut lines = text.lines().collect::<Vec<_>>();
    if text.ends_with('\n') {
        lines.push("");
    }
    lines
}

fn shares_line(old_lines: &[&str], new_lines: &[&str]) -> bool {
    let (shorter, longer) = if old_lines.len() <= new_lines.len() {
        (old_lines, new_lines)
    } else {
        (new_lines, old_lines)
    };
    let lines = shorter.iter().copied().collect::<HashSet<_>>();
    longer.iter().any(|line| lines.contains(line))
}

fn append_large_shared_core<'a>(
    output: &mut Vec<DiffLine<'a>>,
    old_lines: &[&'a str],
    new_lines: &[&'a str],
) {
    if old_lines.len() <= MAX_DIFF_CORE_LINES || new_lines.len() <= MAX_DIFF_CORE_LINES {
        push_omitted(output);
        return;
    }

    let edge_lines = MAX_DIFF_CORE_LINES / 2;
    let old_head = &old_lines[..edge_lines];
    let new_head = &new_lines[..edge_lines];
    if suffix_anchor_matches(old_head, new_head) {
        append_collapsed_ops(output, &diff_ops(old_head, new_head));
    } else {
        push_omitted(output);
    }

    push_omitted(output);

    let old_tail = &old_lines[old_lines.len() - edge_lines..];
    let new_tail = &new_lines[new_lines.len() - edge_lines..];
    if prefix_anchor_matches(old_tail, new_tail) {
        append_collapsed_ops(output, &diff_ops(old_tail, new_tail));
    } else {
        push_omitted(output);
    }
}

fn suffix_anchor_matches(old_lines: &[&str], new_lines: &[&str]) -> bool {
    old_lines[old_lines.len() - CONTEXT_LINES..] == new_lines[new_lines.len() - CONTEXT_LINES..]
}

fn prefix_anchor_matches(old_lines: &[&str], new_lines: &[&str]) -> bool {
    old_lines[..CONTEXT_LINES] == new_lines[..CONTEXT_LINES]
}

fn diff_ops<'a>(old_lines: &[&'a str], new_lines: &[&'a str]) -> Vec<DiffLine<'a>> {
    let columns = new_lines.len() + 1;
    let mut common_lengths = vec![0u16; (old_lines.len() + 1) * columns];
    for old_index in (0..old_lines.len()).rev() {
        for new_index in (0..new_lines.len()).rev() {
            let index = old_index * columns + new_index;
            common_lengths[index] = if old_lines[old_index] == new_lines[new_index] {
                common_lengths[(old_index + 1) * columns + new_index + 1] + 1
            } else {
                common_lengths[(old_index + 1) * columns + new_index]
                    .max(common_lengths[old_index * columns + new_index + 1])
            };
        }
    }

    let mut lines = Vec::with_capacity(old_lines.len() + new_lines.len());
    let (mut old_index, mut new_index) = (0, 0);
    while old_index < old_lines.len() || new_index < new_lines.len() {
        if old_index < old_lines.len()
            && new_index < new_lines.len()
            && old_lines[old_index] == new_lines[new_index]
        {
            lines.push(DiffLine {
                kind: DiffLineKind::Context,
                text: old_lines[old_index],
            });
            old_index += 1;
            new_index += 1;
        } else if old_index < old_lines.len()
            && (new_index == new_lines.len()
                || common_lengths[(old_index + 1) * columns + new_index]
                    >= common_lengths[old_index * columns + new_index + 1])
        {
            lines.push(DiffLine {
                kind: DiffLineKind::Removed,
                text: old_lines[old_index],
            });
            old_index += 1;
        } else {
            lines.push(DiffLine {
                kind: DiffLineKind::Added,
                text: new_lines[new_index],
            });
            new_index += 1;
        }
    }
    lines
}

fn append_collapsed_ops<'a>(output: &mut Vec<DiffLine<'a>>, operations: &[DiffLine<'a>]) {
    let mut index = 0;
    while index < operations.len() {
        if operations[index].kind != DiffLineKind::Context {
            output.push(operations[index]);
            index += 1;
            continue;
        }

        let start = index;
        while index < operations.len() && operations[index].kind == DiffLineKind::Context {
            index += 1;
        }
        let run = &operations[start..index];
        if run.len() <= CONTEXT_LINES * 2 {
            output.extend_from_slice(run);
        } else {
            output.extend_from_slice(&run[..CONTEXT_LINES]);
            push_omitted(output);
            output.extend_from_slice(&run[run.len() - CONTEXT_LINES..]);
        }
    }
}

fn push_omitted<'a>(lines: &mut Vec<DiffLine<'a>>) {
    if lines
        .last()
        .is_none_or(|line| line.kind != DiffLineKind::Omitted)
    {
        lines.push(DiffLine {
            kind: DiffLineKind::Omitted,
            text: "",
        });
    }
}

fn balance_preview<'a>(lines: Vec<DiffLine<'a>>, max_lines: usize) -> Vec<DiffLine<'a>> {
    if lines.len() <= max_lines {
        return lines;
    }
    if max_lines == 0 {
        return Vec::new();
    }

    let removed = indices_for_kind(&lines, DiffLineKind::Removed);
    let added = indices_for_kind(&lines, DiffLineKind::Added);
    let kinds = usize::from(!removed.is_empty()) + usize::from(!added.is_empty());
    if kinds == 0 {
        return vec![DiffLine {
            kind: DiffLineKind::Omitted,
            text: "",
        }];
    }

    let change_budget = max_lines.saturating_sub(1).max(kinds);
    let (mut removed_budget, mut added_budget) = match (removed.is_empty(), added.is_empty()) {
        (false, false) => {
            let removed_budget = change_budget.div_ceil(2);
            (removed_budget, change_budget - removed_budget)
        }
        (false, true) => (change_budget, 0),
        (true, false) => (0, change_budget),
        (true, true) => unreachable!(),
    };

    loop {
        let mut selected = vec![false; lines.len()];
        select_edges(&removed, removed_budget, &mut selected);
        select_edges(&added, added_budget, &mut selected);
        let rebuilt = rebuild_selected(&lines, &selected);
        if rebuilt.len() <= max_lines {
            return rebuilt;
        }

        let minimum_removed = usize::from(!removed.is_empty());
        let minimum_added = usize::from(!added.is_empty());
        if removed_budget > minimum_removed
            && (removed_budget >= added_budget || added_budget == minimum_added)
        {
            removed_budget -= 1;
        } else if added_budget > minimum_added {
            added_budget -= 1;
        } else {
            return vec![DiffLine {
                kind: DiffLineKind::Omitted,
                text: "",
            }];
        }
    }
}

fn indices_for_kind(lines: &[DiffLine<'_>], kind: DiffLineKind) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| (line.kind == kind).then_some(index))
        .collect()
}

fn select_edges(indices: &[usize], budget: usize, selected: &mut [bool]) {
    let budget = budget.min(indices.len());
    let head = budget.div_ceil(2);
    let tail = budget - head;
    for index in indices.iter().take(head) {
        selected[*index] = true;
    }
    for index in indices.iter().rev().take(tail) {
        selected[*index] = true;
    }
}

fn rebuild_selected<'a>(lines: &[DiffLine<'a>], selected: &[bool]) -> Vec<DiffLine<'a>> {
    let mut output = Vec::new();
    let mut omitted = false;
    for (line, selected) in lines.iter().zip(selected) {
        if *selected {
            if omitted {
                push_omitted(&mut output);
                omitted = false;
            }
            output.push(*line);
        } else {
            omitted = true;
        }
    }
    if omitted {
        push_omitted(&mut output);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered<'a>(lines: &'a [DiffLine<'a>]) -> Vec<(DiffLineKind, &'a str)> {
        lines.iter().map(|line| (line.kind, line.text)).collect()
    }

    #[test]
    fn reports_only_changed_lines_with_context() {
        let old = ["before", "same", "old value", "after", "tail"].join("\n");
        let new = ["before", "same", "new value", "after", "tail"].join("\n");
        let lines = preview(Some(&old), &new, false, 32);

        assert_eq!(
            rendered(&lines),
            vec![
                (DiffLineKind::Context, "before"),
                (DiffLineKind::Context, "same"),
                (DiffLineKind::Removed, "old value"),
                (DiffLineKind::Added, "new value"),
                (DiffLineKind::Context, "after"),
                (DiffLineKind::Context, "tail"),
            ]
        );
    }

    #[test]
    fn new_file_reports_additions() {
        let new = ["first", "second"].join("\n");
        let lines = preview(None, &new, false, 32);

        assert_eq!(
            rendered(&lines),
            vec![
                (DiffLineKind::Added, "first"),
                (DiffLineKind::Added, "second"),
            ]
        );
    }

    #[test]
    fn long_unchanged_regions_are_collapsed() {
        let old = (0..20)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>();
        let mut new = old.clone();
        new[3] = "changed 3".to_string();
        new[16] = "changed 16".to_string();
        let old = old.join("\n");
        let new = new.join("\n");
        let lines = preview(Some(&old), &new, false, 32);

        assert!(lines.iter().any(|line| line.kind == DiffLineKind::Omitted));
        assert!(lines
            .iter()
            .any(|line| line.kind == DiffLineKind::Removed && line.text == "line 3"));
        assert!(lines
            .iter()
            .any(|line| line.kind == DiffLineKind::Added && line.text == "changed 16"));
    }

    #[test]
    fn truncated_sources_are_marked_as_partial() {
        let lines = preview(Some("old"), "new", true, 32);

        assert_eq!(lines[0].kind, DiffLineKind::Omitted);
        assert!(lines.iter().any(|line| line.kind == DiffLineKind::Removed));
        assert!(lines.iter().any(|line| line.kind == DiffLineKind::Added));
    }

    #[test]
    fn reports_end_of_file_newline_changes() {
        let added = preview(Some("value"), "value\n", false, 32);
        let removed = preview(Some("value\n"), "value", false, 32);

        assert_eq!(
            rendered(&added),
            vec![(DiffLineKind::Context, "value"), (DiffLineKind::Added, ""),]
        );
        assert_eq!(
            rendered(&removed),
            vec![
                (DiffLineKind::Context, "value"),
                (DiffLineKind::Removed, ""),
            ]
        );
    }

    #[test]
    fn large_unaligned_core_falls_back_to_omission() {
        let unchanged = (0..300)
            .map(|index| format!("same {index}"))
            .collect::<Vec<_>>();
        let old = unchanged.join("\n");
        let mut new = (0..300)
            .map(|index| format!("inserted {index}"))
            .collect::<Vec<_>>();
        new.extend(unchanged);
        new.push("changed tail".to_string());
        let new = new.join("\n");

        let lines = preview(Some(&old), &new, false, 32);

        assert_eq!(rendered(&lines), vec![(DiffLineKind::Omitted, "")]);
    }

    #[test]
    fn large_aligned_core_keeps_head_and_tail_edits() {
        let old = (0..300)
            .map(|index| format!("same {index}"))
            .collect::<Vec<_>>();
        let mut new = old.clone();
        new[0] = "changed head".to_string();
        new[299] = "changed tail".to_string();
        let old = old.join("\n");
        let new = new.join("\n");

        let lines = preview(Some(&old), &new, false, 32);

        assert!(lines
            .iter()
            .any(|line| line.kind == DiffLineKind::Removed && line.text == "same 0"));
        assert!(lines
            .iter()
            .any(|line| line.kind == DiffLineKind::Added && line.text == "changed head"));
        assert!(lines
            .iter()
            .any(|line| line.kind == DiffLineKind::Removed && line.text == "same 299"));
        assert!(lines
            .iter()
            .any(|line| line.kind == DiffLineKind::Added && line.text == "changed tail"));
        assert!(lines.iter().any(|line| line.kind == DiffLineKind::Omitted));
    }

    #[test]
    fn replacement_heavy_preview_balances_removed_and_added_lines() {
        let old = (0..100)
            .map(|index| format!("old {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let new = (0..100)
            .map(|index| format!("new {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = preview(Some(&old), &new, false, 20);
        let removed = lines
            .iter()
            .filter(|line| line.kind == DiffLineKind::Removed)
            .count();
        let added = lines
            .iter()
            .filter(|line| line.kind == DiffLineKind::Added)
            .count();

        assert!(lines.len() <= 20);
        assert!(removed > 0);
        assert!(added > 0);
        assert!(removed.abs_diff(added) <= 1);
        assert_eq!(lines[0].kind, DiffLineKind::Removed);
        assert!(lines.iter().any(|line| line.kind == DiffLineKind::Omitted));
        assert_eq!(
            lines.last().map(|line| line.kind),
            Some(DiffLineKind::Added)
        );
    }

    #[test]
    fn one_sided_preview_uses_the_available_change_budget() {
        let new = (0..100)
            .map(|index| format!("new {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let old = new.replace("new", "old");
        for (lines, expected) in [
            (preview(None, &new, false, 12), DiffLineKind::Added),
            (preview(Some(&old), "", false, 12), DiffLineKind::Removed),
        ] {
            assert!(lines.len() <= 12);
            assert!(lines
                .iter()
                .all(|line| matches!(line.kind, DiffLineKind::Omitted) || line.kind == expected));
            assert!(lines.iter().filter(|line| line.kind == expected).count() >= 8);
        }
    }

    #[test]
    fn undersized_budget_never_truncates_one_change_kind() {
        let old = ["old first", "old second"].join("\n");
        let new = ["new first", "new second"].join("\n");
        let lines = preview(Some(&old), &new, false, 2);

        assert_eq!(rendered(&lines), vec![(DiffLineKind::Omitted, "")]);
    }

    #[test]
    fn multi_hunk_budget_accounts_for_every_omission() {
        let lines = vec![
            DiffLine {
                kind: DiffLineKind::Removed,
                text: "old first",
            },
            DiffLine {
                kind: DiffLineKind::Context,
                text: "same first",
            },
            DiffLine {
                kind: DiffLineKind::Added,
                text: "new first",
            },
            DiffLine {
                kind: DiffLineKind::Context,
                text: "same second",
            },
            DiffLine {
                kind: DiffLineKind::Removed,
                text: "old second",
            },
            DiffLine {
                kind: DiffLineKind::Context,
                text: "same third",
            },
            DiffLine {
                kind: DiffLineKind::Added,
                text: "new second",
            },
        ];

        let balanced = balance_preview(lines, 4);

        assert!(balanced.len() <= 4);
        assert!(balanced
            .iter()
            .any(|line| line.kind == DiffLineKind::Removed));
        assert!(balanced.iter().any(|line| line.kind == DiffLineKind::Added));
        assert!(balanced
            .iter()
            .any(|line| line.kind == DiffLineKind::Omitted));
    }
}
