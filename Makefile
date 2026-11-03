SHELL := /usr/bin/env bash

.PHONY: help fmt fmt-check clippy test build lint-md npm-check ci install-hooks clean

# Delegate every target to the justfile
help:
	@just --list

fmt:
	@just fmt

fmt-check:
	@just fmt-check

clippy:
	@just clippy

test:
	@just test

build:
	@just build

lint-md:
	@just lint-md

npm-check:
	@just npm-check

ci:
	@just ci

install-hooks:
	@just install-hooks

clean:
	@just clean
