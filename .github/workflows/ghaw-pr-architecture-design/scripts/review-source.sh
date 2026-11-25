#!/usr/bin/env bash
set -euo pipefail

if ! [[ "${REVIEW_BASE:-}" =~ ^[0-9a-f]{40}$ && "${REVIEW_HEAD:-}" =~ ^[0-9a-f]{40}$ ]]; then
    echo 'Review revisions are unavailable.' >&2
    exit 1
fi
revision() {
    case "$1" in
        base) printf '%s' "$REVIEW_BASE" ;;
        head) printf '%s' "$REVIEW_HEAD" ;;
        *) echo 'Revision must be base or head.' >&2; return 2 ;;
    esac
}

export GIT_NO_REPLACE_OBJECTS=1 GIT_LITERAL_PATHSPECS=1
case "${1:-}:$#" in
    files:1)
        git --no-pager diff --no-ext-diff --no-textconv --name-status "$REVIEW_BASE" "$REVIEW_HEAD" --
        ;;
    diff:1|diff:2)
        shift
        git --no-pager diff --no-ext-diff --no-textconv --no-color "$REVIEW_BASE" "$REVIEW_HEAD" -- "$@"
        ;;
    source:3)
        sha="$(revision "$2")"
        git --no-pager cat-file blob "$sha:$3"
        ;;
    search:3)
        sha="$(revision "$2")"
        git --no-pager grep --no-textconv -I -n -F -e "$3" "$sha" --
        ;;
    *)
        echo 'Usage: review-source.sh files | diff [path] | source base|head path | search base|head literal' >&2
        exit 2
        ;;
esac
