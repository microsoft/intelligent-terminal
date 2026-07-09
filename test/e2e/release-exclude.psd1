# Items intentionally excluded from the generated release checklist (release-report.md).
#
# Each entry is a regex matched (case-insensitive) against a checklist item's bold title.
# Matching items are dropped entirely from the report — not counted as passed or manual — so
# the checklist stays focused on the tests we actually care about for sign-off. The canonical
# doc/release-check-list.md is left untouched; this only filters the generated report.
@{
    Exclude = @(
        # RTL / right-to-left mirrored layout — not part of the focused sign-off set.
        'RTL'
    )
}
