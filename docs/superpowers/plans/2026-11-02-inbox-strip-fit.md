# Inbox strip fit Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline) or superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Inbox title strip never truncates “Inbox” or clips filters down to sidebar width 260.

**Architecture:** Pure `inbox_strip_fit` decides stack / counts / mark-all from width; `title_strip` and `segments` consume it. Fixed “Inbox” label is `flex_none`.

**Tech Stack:** Rust, GPUI, existing sidebar strip components.

**Spec:** `docs/superpowers/specs/2026-11-02-inbox-strip-fit-design.md`

## Global Constraints

- Sidebar range stays `(260., 420.)`.
- Arithmetic thresholds only; no per-frame text measurement.
- Phone: no mark-all in strip; stack when text scale > 1.25.

## Review Focus

- Default 316 with unread + mentions + mark-all: title intact, counts hidden, mark-all visible.
- Width 260 with unread: stacked, title intact, filters intact.
- Width 400: counts visible.
- Phone large text: still stacks; mark-all still absent from strip.
- Discord/Slack segment counts still appear at ≥ 300.

## Task 1: Fit helper + tests

**Files:** `tinyclient/crates/tinyclient/src/gui/sidebar.rs` (or small helper next to strip code)

- [x] Add `InboxStripFit` + `inbox_strip_fit` with the spec thresholds.
- [x] Unit tests covering the four width bands and phone large-text stack.
- [x] Run tests; confirm fail before wiring if extracted first, then pass.

## Task 2: Wire title strip

**Files:** `sidebar.rs` (`segments`, `title_strip` Inbox arm); `docs/design/polish/04-unified.md`

- [x] `segments` takes `show_counts: bool` instead of computing `roomy` for Inbox; Discord/Slack pass `width >= 300`.
- [x] Inbox uses fit: fixed title, stack path includes mark-all on the title row when allowed.
- [x] Update polish/04 one-liner for the new thresholds.
- [x] `cargo test` for the crate (or targeted sidebar tests).
