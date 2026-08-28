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
    let old_lines = old_text.map_or_else(Vec::new, |text| text.lines().collect());
    let new_lines = new_text.lines().collect::<Vec<_>>();
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
    let old_bounded = &old_core[..old_core.len().min(MAX_DIFF_CORE_LINES)];
    let new_bounded = &new_core[..new_core.len().min(MAX_DIFF_CORE_LINES)];
    append_collapsed_ops(&mut lines, &diff_ops(old_bounded, new_bounded));
    if old_core.len() > old_bounded.len() || new_core.len() > new_bounded.len() {
        push_omitted(&mut lines);
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

    // Each selected kind contributes at most two contiguous ranges (head and
    // tail), so reserving `2 * kinds + 1` rows covers every possible gap.
    let change_budget = max_lines.saturating_sub(2 * kinds + 1).max(kinds);
    let (removed_budget, added_budget) = match (removed.is_empty(), added.is_empty()) {
        (false, false) => {
            let removed_budget = change_budget.div_ceil(2);
            (removed_budget, change_budget - removed_budget)
        }
        (false, true) => (change_budget, 0),
        (true, false) => (0, change_budget),
        (true, true) => unreachable!(),
    };

    let mut selected = vec![false; lines.len()];
    select_edges(&removed, removed_budget, &mut selected);
    select_edges(&added, added_budget, &mut selected);
    rebuild_selected(&lines, &selected, max_lines)
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

fn rebuild_selected<'a>(
    lines: &[DiffLine<'a>],
    selected: &[bool],
    max_lines: usize,
) -> Vec<DiffLine<'a>> {
    let mut output = Vec::with_capacity(max_lines);
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
    output.truncate(max_lines);
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
        let lines = preview(
            Some("before\nsame\nold value\nafter\ntail"),
            "before\nsame\nnew value\nafter\ntail",
            false,
            32,
        );

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
        let lines = preview(None, "first\nsecond", false, 32);

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
}
