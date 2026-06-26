set shell := ["bash", "-cu"]

default_editor := "Code - OSS"
default_config := "run/code-oss.toml"
default_profile := ""

_run_dir:
    mkdir -p "$(dirname "{{default_config}}")"

build:
    cargo build

test:
    cargo test

fmt:
    cargo fmt

fmt-check:
    cargo fmt --check

clippy:
    cargo clippy --all-targets -- -D warnings

check: fmt-check clippy test

detect:
    cargo run -- detect

run *args:
    cargo run -- {{args}}

dev *args: _run_dir
    cargo run -- --config "{{default_config}}" --editor "{{default_editor}}" {{args}}

init profile=default_profile: _run_dir
    profile_args=(); if [ -n "{{profile}}" ]; then profile_args=(--profile "{{profile}}"); fi; cargo run -- --config "{{default_config}}" --editor "{{default_editor}}" "${profile_args[@]}" init

list-profiles:
    cargo run -- --config "{{default_config}}" --editor "{{default_editor}}" list-profiles

status profile=default_profile: _run_dir
    profile_args=(); if [ -n "{{profile}}" ]; then profile_args=(--profile "{{profile}}"); fi; cargo run -- --config "{{default_config}}" --editor "{{default_editor}}" "${profile_args[@]}" status

push profile=default_profile: _run_dir
    profile_args=(); if [ -n "{{profile}}" ]; then profile_args=(--profile "{{profile}}"); fi; cargo run -- --config "{{default_config}}" --editor "{{default_editor}}" "${profile_args[@]}" push

pull profile=default_profile: _run_dir
    profile_args=(); if [ -n "{{profile}}" ]; then profile_args=(--profile "{{profile}}"); fi; cargo run -- --config "{{default_config}}" --editor "{{default_editor}}" "${profile_args[@]}" pull

sync profile=default_profile: _run_dir
    profile_args=(); if [ -n "{{profile}}" ]; then profile_args=(--profile "{{profile}}"); fi; cargo run -- --config "{{default_config}}" --editor "{{default_editor}}" "${profile_args[@]}" sync

smoke:
    gh workflow run vscodium-smoke.yml
