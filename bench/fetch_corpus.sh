#!/bin/sh
# Fetches the benchmark corpus (907 public test PDFs, 130 MB) into a directory and proves it has the bytes the
# benchmarks were measured on: every file is named in bench/sets/corpus.lock with its source repository, the
# commit it is taken from, and its sha256.
#
#   bench/fetch_corpus.sh [DIR]            fetch what is missing or wrong into DIR (default: $CORPUS, else bench/corpus)
#   bench/fetch_corpus.sh --check [DIR]    only check: every file present with the lock's sha256?
#   JOBS=N bench/fetch_corpus.sh ...       downloads at a time (default 8)
#
# Needs curl, sha256sum (or shasum), awk and a POSIX shell; HTTPS_PROXY is honoured by curl. A download is
# hashed before it is put in place and kept only when its sha256 is the lock's, so a stopped run continues
# where it was and a file that changed upstream is reported, never used. The files are untrusted test input:
# they are written and hashed, never opened. Exit status 1 when anything is missing or wrong at the end.
set -u

here=$(cd "$(dirname "$0")" && pwd)
lock="$here/sets/corpus.lock"
mode=fetch
dir=""
for arg in "$@"; do
    case "$arg" in
        --check) mode=check ;;
        -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
        -*) echo "fetch_corpus.sh: unknown option $arg" >&2; exit 2 ;;
        *) dir="$arg" ;;
    esac
done
[ -n "$dir" ] || dir="${CORPUS:-$here/corpus}"
jobs="${JOBS:-8}"
[ -f "$lock" ] || { echo "fetch_corpus.sh: no $lock" >&2; exit 2; }

if command -v sha256sum >/dev/null 2>&1; then
    hash_of() { sha256sum "$1" | cut -c1-64; }
elif command -v shasum >/dev/null 2>&1; then
    hash_of() { shasum -a 256 "$1" | cut -c1-64; }
else
    echo "fetch_corpus.sh: need sha256sum or shasum" >&2; exit 2
fi

# A member path as a URL: only the space needs escaping in this corpus; the lock allows no other special character.
encode() { printf '%s' "$1" | sed 's/ /%20/g'; }

# One file: 0 when it is present with the right sha256 (fetched now or before), 1 otherwise.
fetch_one() {
    path=$1 repo=$2 commit=$3 member=$4 sha=$5 size=$6
    target="$dir/$path"
    if [ -f "$target" ] && [ "$(hash_of "$target")" = "$sha" ]; then
        return 0
    fi
    if [ "$mode" = check ]; then
        echo "WRONG OR MISSING $path" >&2
        return 1
    fi
    mkdir -p "$(dirname "$target")"
    tmp="$target.part.$$"
    url="https://raw.githubusercontent.com/$repo/$commit/$(encode "$member")"
    try=0
    while [ $try -lt 4 ]; do
        try=$((try + 1))
        if curl -fsSL --retry 2 -A "gpress-fetch-corpus/1" -o "$tmp" "$url"; then
            got=$(hash_of "$tmp")
            if [ "$got" = "$sha" ]; then
                mv "$tmp" "$target"
                echo "fetched $path"
                return 0
            fi
            echo "WRONG sha256 for $path from $url (got $got)" >&2
            rm -f "$tmp"
            return 1
        fi
        sleep $((try * 2))
    done
    rm -f "$tmp"
    echo "FAILED to fetch $path from $url" >&2
    return 1
}

# The rows, tab-separated, without the comments and the header, in a file: the loop reads them in this shell
# (a loop fed by a pipe runs in a subshell, whose background jobs the parent's `wait` cannot see, so the last
# batch would be reported before it finished). Each row runs in the background, at most $jobs at a time;
# failures are counted through a file (a background job can't change this shell's variables).
rows=$(mktemp)
fails=$(mktemp)
trap 'rm -f "$rows" "$fails"' EXIT
awk -F'\t' '!/^#/ && $1 != "path" && NF == 6' "$lock" >"$rows"
: >"$fails"
running=0
while IFS="$(printf '\t')" read -r path repo commit member sha size; do
    # The job's stdin is not the rows file, so nothing it runs can eat the rows.
    { fetch_one "$path" "$repo" "$commit" "$member" "$sha" "$size" || echo "$path" >>"$fails"; } </dev/null &
    running=$((running + 1))
    if [ "$running" -ge "$jobs" ]; then
        wait
        running=0
    fi
done <"$rows"
wait

n=$(wc -l <"$rows" | tr -d ' ')
bad=$(wc -l <"$fails" | tr -d ' ')
if [ "$bad" -eq 0 ]; then
    echo "corpus ok: $n files in $dir, every sha256 as in sets/corpus.lock"
    exit 0
fi
echo "corpus NOT complete: $bad of $n files missing or wrong (see above)" >&2
exit 1
