default:
  @just --list --justfile {{justfile()}}

build_agent:
  #!/usr/bin/env bash
  set -e
  cargo build
  setcap -q -v "cap_sys_admin,cap_chown,cap_setpcap=p" ./target/debug/nixless-agent || sudo setcap "cap_sys_admin,cap_chown,cap_setpcap=p" ./target/debug/nixless-agent
