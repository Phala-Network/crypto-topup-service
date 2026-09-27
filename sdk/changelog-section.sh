#!/bin/sh
set -eu

# Prints the release notes of VERSION from a Keep a Changelog 1.1.0 file: the body of its
# `## [VERSION] - YYYY-MM-DD` section, without surrounding blank lines. Fails when the section is
# missing, undated, or empty, so a release cannot be tagged before its changelog entry.
if [ "$#" -ne 2 ]; then
    echo "usage: changelog-section.sh CHANGELOG VERSION" >&2
    exit 64
fi

awk -v heading="## [$2] - " '
    /^## / || /^\[[^]]+\]: / {
        if (found) { done = 1 }
        else if (index($0, heading) == 1 && substr($0, length(heading) + 1) ~ /^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]$/) { found = 1; next }
    }
    found && !done {
        if ($0 ~ /^[[:space:]]*$/) { blank++; next }
        if (printed) { while (blank-- > 0) print "" }
        blank = 0; printed = 1; print
    }
    END {
        if (!printed) { printf "%s has no dated, non-empty section %s\n", FILENAME, heading "YYYY-MM-DD" > "/dev/stderr"; exit 1 }
    }
' "$1"
