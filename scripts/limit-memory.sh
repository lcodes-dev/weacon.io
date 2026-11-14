#!/usr/bin/env bash
# Runs a test binary with at most 4 GiB of virtual memory.
ulimit -v 4194304
exec "$@"
