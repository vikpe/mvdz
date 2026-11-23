#!/bin/bash
# Fetch a benchmark sample: the raw demos of one list in benchmarks/samples/,
# named <sha256>.mvd, from $MVDZ_SAMPLES_URL/<set>.tar.gz.
#
#   scripts/fetch_samples.sh SET DIR     (SET: sample8, sample40 or stratified)
#
# Unpacks into DIR, checks the SHA-256 of every demo against its name and
# reports the demos of the list that the tarball lacks (DIR/MISSING.txt).
# Environment: MVDZ_SAMPLES_URL (default https://dev.quake.world/mvdz).
set -eu
if [ $# -ne 2 ]; then
  echo "usage: $0 sample8|sample40|stratified DIR" >&2
  exit 2
fi
SET=$1
DIR=$2
BASE=${MVDZ_SAMPLES_URL:-https://dev.quake.world/mvdz}
SAMPLES=$(cd "$(dirname "$0")/../benchmarks/samples" && pwd)
case $SET in
  sample8 | sample40) LIST=$SAMPLES/$SET.txt ;;
  stratified) LIST=$SAMPLES/$SET.tsv ;;
  *)
    echo "unknown set: $SET (sample8, sample40 or stratified)" >&2
    exit 2
    ;;
esac
mkdir -p "$DIR"
curl -fsSL --retry 3 "$BASE/$SET.tar.gz" | tar -xz -C "$DIR" --strip-components=1
cd "$DIR"
# every demo must hash to its name
for f in *.mvd; do echo "${f%.mvd}  $f"; done | sha256sum -c --quiet
listed=$(grep -v '^#' "$LIST" | grep -oE '^[0-9a-f]{64}' | sort -u)
missing=$(for h in $listed; do [ -f "$h.mvd" ] || echo "$h"; done)
n_listed=$(echo "$listed" | wc -l)
n_missing=$(echo -n "$missing" | grep -c . || true)
echo "$SET: $((n_listed - n_missing)) of $n_listed demos in $DIR, SHA-256 checked"
if [ "$n_missing" -gt 0 ]; then
  echo "$SET: $n_missing listed demos are not in the tarball (see $DIR/MISSING.txt)"
fi
