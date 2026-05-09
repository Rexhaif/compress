#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CACHE="$ROOT/bench/cache"
DOWNLOADS="$CACHE/downloads"
INPUTS="$CACHE/inputs"

CANTERBURY_BASE="https://corpus.canterbury.ac.nz/resources"
MAHONEY_BASE="https://www.mattmahoney.net/dc"
PIZZACHILI_BASE="https://pizzachili.dcc.uchile.cl"
SILESIA_URL="https://sun.aei.polsl.pl/~sdeor/corpus/silesia.zip"

declare -a TARGETS=()
declare -a PREPARED_PATHS=()

usage() {
    cat <<'EOF'
Usage: bench/fetch-corpora.sh [group|corpus ...]

Groups:
  standard              Canterbury, Calgary, Large Canterbury, Artificial,
                        Silesia, enwik8, and practical Pizza&Chili samples
  canterbury-suite      Canterbury, Calgary, Large Canterbury, Artificial
  pizzachili-50mb       Pizza&Chili english/source/dna/protein/xml 50 MiB files
  pizzachili-repetitive Small practical samples from Pizza&Chili repetitive/logs
  release               enwik9 and larger Pizza&Chili 100 MiB files
  all                   standard plus release

Individual corpora:
  canterbury
  calgary
  canterbury-large
  canterbury-artificial
  silesia
  enwik8
  enwik9
  pch-english-50mb
  pch-sources-50mb
  pch-dna-50mb
  pch-proteins-50mb
  pch-dblp-50mb
  pch-english-100mb
  pch-sources-100mb
  pch-dna-100mb
  pch-proteins-100mb
  pch-dblp-100mb
  pch-world-leaders
  pch-android-log

Prepared files are written under bench/cache. Multi-file corpora are also
packed into bench/cache/inputs/*.tar so bench/run can consume them directly.
EOF
}

log() {
    printf '[fetch-corpora] %s\n' "$*"
}

need() {
    if ! command -v "$1" >/dev/null 2>&1; then
        printf 'bench/fetch-corpora.sh: missing required command: %s\n' "$1" >&2
        exit 1
    fi
}

add_target() {
    local target="$1"
    local existing
    for existing in "${TARGETS[@]}"; do
        if [[ "$existing" == "$target" ]]; then
            return
        fi
    done
    TARGETS+=("$target")
}

add_group() {
    case "$1" in
        standard)
            add_group canterbury-suite
            add_target silesia
            add_target enwik8
            add_group pizzachili-50mb
            add_group pizzachili-repetitive
            ;;
        canterbury-suite)
            add_target canterbury
            add_target calgary
            add_target canterbury-large
            add_target canterbury-artificial
            ;;
        pizzachili-50mb)
            add_target pch-english-50mb
            add_target pch-sources-50mb
            add_target pch-dna-50mb
            add_target pch-proteins-50mb
            add_target pch-dblp-50mb
            ;;
        pizzachili-repetitive)
            add_target pch-world-leaders
            add_target pch-android-log
            ;;
        release)
            add_target enwik9
            add_target pch-english-100mb
            add_target pch-sources-100mb
            add_target pch-dna-100mb
            add_target pch-proteins-100mb
            add_target pch-dblp-100mb
            ;;
        all)
            add_group standard
            add_group release
            ;;
        *)
            return 1
            ;;
    esac
}

download() {
    local url="$1"
    local output="$2"
    mkdir -p "$(dirname "$output")"

    if [[ -s "$output" ]]; then
        log "cached $(realpath --relative-to="$ROOT" "$output")"
        return
    fi

    log "download $url"
    curl --location --fail --show-error --retry 3 --retry-delay 2 \
        --continue-at - --output "$output.part" "$url"
    mv "$output.part" "$output"
}

record_prepared() {
    local path="$1"
    PREPARED_PATHS+=("$path")
}

stable_tar() {
    local source_dir="$1"
    local output="$2"
    mkdir -p "$(dirname "$output")"

    log "pack $(realpath --relative-to="$ROOT" "$output")"
    tar --sort=name \
        --mtime='UTC 1970-01-01' \
        --owner=0 \
        --group=0 \
        --numeric-owner \
        -cf "$output.part" \
        -C "$source_dir" .
    mv "$output.part" "$output"
    record_prepared "$output"
}

extract_tar_corpus() {
    local name="$1"
    local url="$2"
    local archive="$DOWNLOADS/$name.tar.gz"
    local output_dir="$CACHE/$name"

    download "$url" "$archive"
    if [[ ! -d "$output_dir" ]] || [[ -z "$(find "$output_dir" -mindepth 1 -print -quit 2>/dev/null)" ]]; then
        log "extract $(realpath --relative-to="$ROOT" "$archive")"
        rm -rf "$output_dir.tmp"
        mkdir -p "$output_dir.tmp"
        tar -xzf "$archive" -C "$output_dir.tmp"
        rm -rf "$output_dir"
        mv "$output_dir.tmp" "$output_dir"
    fi

    stable_tar "$output_dir" "$INPUTS/$name.tar"
}

extract_zip_corpus() {
    local name="$1"
    local url="$2"
    local archive="$DOWNLOADS/$name.zip"
    local output_dir="$CACHE/$name"

    download "$url" "$archive"
    if [[ ! -d "$output_dir" ]] || [[ -z "$(find "$output_dir" -mindepth 1 -print -quit 2>/dev/null)" ]]; then
        log "extract $(realpath --relative-to="$ROOT" "$archive")"
        rm -rf "$output_dir.tmp"
        mkdir -p "$output_dir.tmp"
        unzip -q "$archive" -d "$output_dir.tmp"
        rm -rf "$output_dir"
        mv "$output_dir.tmp" "$output_dir"
    fi

    stable_tar "$output_dir" "$INPUTS/$name.tar"
}

extract_zip_file() {
    local name="$1"
    local url="$2"
    local member="$3"
    local output="$4"
    local archive="$DOWNLOADS/$name.zip"

    download "$url" "$archive"
    if [[ ! -s "$output" ]]; then
        mkdir -p "$(dirname "$output")"
        log "extract $(realpath --relative-to="$ROOT" "$output")"
        unzip -p "$archive" "$member" > "$output.part"
        mv "$output.part" "$output"
    fi
    record_prepared "$output"
}

extract_gzip_file() {
    local name="$1"
    local url="$2"
    local output="$3"
    local archive="$DOWNLOADS/$name.gz"

    download "$url" "$archive"
    if [[ ! -s "$output" ]]; then
        mkdir -p "$(dirname "$output")"
        log "extract $(realpath --relative-to="$ROOT" "$output")"
        gzip -dc "$archive" > "$output.part"
        mv "$output.part" "$output"
    fi
    record_prepared "$output"
}

fetch_target() {
    case "$1" in
        canterbury)
            extract_tar_corpus canterbury "$CANTERBURY_BASE/cantrbry.tar.gz"
            ;;
        calgary)
            extract_tar_corpus calgary "$CANTERBURY_BASE/calgary.tar.gz"
            ;;
        canterbury-large)
            extract_tar_corpus canterbury-large "$CANTERBURY_BASE/large.tar.gz"
            ;;
        canterbury-artificial)
            extract_tar_corpus canterbury-artificial "$CANTERBURY_BASE/artificl.tar.gz"
            ;;
        silesia)
            extract_zip_corpus silesia "$SILESIA_URL"
            ;;
        enwik8)
            extract_zip_file enwik8 "$MAHONEY_BASE/enwik8.zip" enwik8 "$CACHE/enwik8"
            ;;
        enwik9)
            extract_zip_file enwik9 "$MAHONEY_BASE/enwik9.zip" enwik9 "$CACHE/enwik9"
            ;;
        pch-english-50mb)
            extract_gzip_file pch-english-50mb "$PIZZACHILI_BASE/texts/nlang/english.50MB.gz" "$CACHE/pizzachili/english.50MB"
            ;;
        pch-sources-50mb)
            extract_gzip_file pch-sources-50mb "$PIZZACHILI_BASE/texts/code/sources.50MB.gz" "$CACHE/pizzachili/sources.50MB"
            ;;
        pch-dna-50mb)
            extract_gzip_file pch-dna-50mb "$PIZZACHILI_BASE/texts/dna/dna.50MB.gz" "$CACHE/pizzachili/dna.50MB"
            ;;
        pch-proteins-50mb)
            extract_gzip_file pch-proteins-50mb "$PIZZACHILI_BASE/texts/protein/proteins.50MB.gz" "$CACHE/pizzachili/proteins.50MB"
            ;;
        pch-dblp-50mb)
            extract_gzip_file pch-dblp-50mb "$PIZZACHILI_BASE/texts/xml/dblp.xml.50MB.gz" "$CACHE/pizzachili/dblp.xml.50MB"
            ;;
        pch-english-100mb)
            extract_gzip_file pch-english-100mb "$PIZZACHILI_BASE/texts/nlang/english.100MB.gz" "$CACHE/pizzachili/english.100MB"
            ;;
        pch-sources-100mb)
            extract_gzip_file pch-sources-100mb "$PIZZACHILI_BASE/texts/code/sources.100MB.gz" "$CACHE/pizzachili/sources.100MB"
            ;;
        pch-dna-100mb)
            extract_gzip_file pch-dna-100mb "$PIZZACHILI_BASE/texts/dna/dna.100MB.gz" "$CACHE/pizzachili/dna.100MB"
            ;;
        pch-proteins-100mb)
            extract_gzip_file pch-proteins-100mb "$PIZZACHILI_BASE/texts/protein/proteins.100MB.gz" "$CACHE/pizzachili/proteins.100MB"
            ;;
        pch-dblp-100mb)
            extract_gzip_file pch-dblp-100mb "$PIZZACHILI_BASE/texts/xml/dblp.xml.100MB.gz" "$CACHE/pizzachili/dblp.xml.100MB"
            ;;
        pch-world-leaders)
            extract_gzip_file pch-world-leaders "$PIZZACHILI_BASE/repcorpus/real/world_leaders.gz" "$CACHE/pizzachili/repetitive/world_leaders"
            ;;
        pch-android-log)
            extract_gzip_file pch-android-log "$PIZZACHILI_BASE/repcorpus/logs/Android.gz" "$CACHE/pizzachili/repetitive/Android"
            ;;
        *)
            printf 'bench/fetch-corpora.sh: unknown corpus: %s\n' "$1" >&2
            exit 1
            ;;
    esac
}

write_manifest() {
    local manifest="$CACHE/manifest.tsv"
    local path
    local bytes
    local hash

    log "write $(realpath --relative-to="$ROOT" "$manifest")"
    {
        printf 'path\tbytes\tsha256\n'
        for path in "${PREPARED_PATHS[@]}"; do
            if [[ -f "$path" ]]; then
                bytes="$(wc -c < "$path" | tr -d ' ')"
                hash="$(sha256sum "$path" | awk '{print $1}')"
                printf '%s\t%s\t%s\n' "$(realpath --relative-to="$ROOT" "$path")" "$bytes" "$hash"
            fi
        done
    } > "$manifest"
}

main() {
    if [[ "${1:-}" == "--help" ]] || [[ "${1:-}" == "-h" ]]; then
        usage
        exit 0
    fi

    if [[ "${1:-}" == "--list" ]]; then
        usage
        exit 0
    fi

    need curl
    need gzip
    need realpath
    need sha256sum
    need tar
    need unzip

    mkdir -p "$CACHE" "$DOWNLOADS" "$INPUTS"

    if [[ "$#" -eq 0 ]]; then
        add_group standard
    else
        local arg
        for arg in "$@"; do
            arg="${arg#--}"
            if ! add_group "$arg"; then
                add_target "$arg"
            fi
        done
    fi

    local target
    for target in "${TARGETS[@]}"; do
        fetch_target "$target"
    done

    write_manifest
}

main "$@"
