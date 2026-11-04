# Sign-in switch, client side: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The first-run screen and Server Settings › Privacy let the owner pick Privacy, pick one of three sign-in methods, see the steps left before it is safe, add their own passkey and hand out passkey links, all from the client; and every member manages their own passkeys from Server Settings › Sign-in.

**Architecture:** `protocol` learns the server's new fields and five IPC requests. `chatd` adds the HTTP calls and the two new refusals. `tiny_setup.rs` gets a pure `Checklist` model with tests. The GUI draws the checklist under the picked sign-in option, a Passkey-links modal, and a Passkeys section on the Sign-in page.

**Tech Stack:** Rust, gpui, serde. Tests: `cargo test -p protocol`, `cargo test -p tinyclient tiny_setup`, `cargo test -p chatd manage --no-default-features` (chatd needs protoc otherwise; see memory `chatd-tests-need-protoc`), GUI compile `cargo check -p tinyclient --features gui`.

**Spec:** `../../../_wt-signin-server/docs/superpowers/specs/2026-11-03-sign-in-switch.md` (lives in the tinyserver worktree; copy it to `docs/superpowers/specs/` here in Task 1 so it travels with this branch).

**Worktree:** `C:\Users\gollum\Development\Personal\tiny\_wt-signin-client`, branch `feat/sign-in-switch` off `origin/main` (cc98775). The server contract is the server plan's Task 5; until that is merged, tests here use the JSON shapes written in that plan.

## Global Constraints

- Copy: the server's words for options and effects; the client writes only its own UI words. Plain English, no "SSO", no "OIDC". Provider is named by `server.provider_name` / the community's provider words as the Sign-in page already does.
- An older server (no `members`, no `owner_has_provider`, `deployment.passkeys` possibly false) must still work: every new protocol field is `#[serde(default, deserialize_with = "or_default")]` like its neighbours; `blocked()` keeps its passkeys reason.
- Claim links and recovery codes are shown once and never written to disk or logs; chatd passes them through, the GUI holds them only in the open modal's state.
- Ids for new controls use the existing prefixes: `tiny-setup-*` / `tiny-privacy-*` on the two setup screens, `tiny-signin-*` on the Sign-in page.
- No new crates.

## Review Focus

1. Owner picks Privacy on a server where they have no passkey and 3 members lack one, presses Apply: the server refuses `owner_needs_passkey`; the checklist must already have told them, and "Add a passkey for me" must be the first action, not buried in a refusal. Pinned in Task 3.
2. Owner picks *Through Pocket ID only* after running passkeys-only for a while: the mirror checklist shows members who have no provider identity and "Switch anyway" is Danger. Pinned in Task 3.
3. A server that answers 404 to `/v1/setup/claim-links` (older server): the modal says the `OLDER` words and offers nothing else. Pinned in Task 2.
4. Removing the last passkey when no recovery code is left: the server refuses; the client must disable Remove with the reason rather than show a failure toast. Pinned in Task 5.
5. The member list in the modal when every member already has a passkey: the "Send passkey links…" button is not shown at all. Pinned in Task 3.

---

### Task 1: Protocol — the server's new words and the five requests

**Files:**
- Create: `docs/superpowers/specs/2026-11-03-sign-in-switch.md` (copy)
- Modify: `crates/protocol/src/tiny.rs:801` (`TINY_SIGN_IN_METHODS`), `:854-866` (`TinySetupCounts`), `:916-938` (`TinySetup`), `:972-980` (`TinySetupRefusal`), new structs after `TinySetupApplied`
- Modify: `crates/protocol/src/lib.rs` after `TinyPasskeyLink` (:1700) and after `TinySetupRefused` (:2079)
- Test: `crates/protocol/src/tiny.rs` tests from :1698

**Interfaces (produces):**
- `pub const TINY_SIGN_IN_METHODS: [&str; 3] = ["provider", "any", "passkey"];`
- `TinySetupCounts` += `members_without_provider: u32`, `owner_has_provider: bool`.
- `pub struct TinySetupMember { user_id: String, display_name: String, passkey: bool, provider: bool }`; `TinySetup.members: Vec<TinySetupMember>` (read with `readable_rows`).
- `TinySetupRefusal` += `OwnerNeedsProvider`, `MembersWithoutProvider { count: u32 }`. Keep `PasskeysUnavailable` (older servers).
- `pub struct TinyClaimLink { user_id: String, display_name: String, url: String }`.
- `pub struct TinyPasskey { id: String, label: String, created_at: String, last_used_at: Option<String>, synced: bool, flagged: bool }`; `pub struct TinyPasskeys { passkeys: Vec<TinyPasskey>, recovery_codes_left: u32 }`.
- Requests: `TinySetupClaimLinks { account, user_ids: Vec<String> }` → `Response::TinyClaimLinks { links: Vec<TinyClaimLink> }`; `TinyPasskeysRead { account }` → `Response::TinyPasskeys { passkeys: TinyPasskeys }`; `TinyPasskeyRename { account, id, label }` → `Ok`; `TinyPasskeyRemove { account, id }` → `Ok`; `TinyRecoveryCodes { account }` → `Response::TinyRecoveryCodes { codes: Vec<String> }`. Doc comments in the style of `TinyPasskeyLink`'s.

- [ ] **Step 1: Write the failing tests** — extend `SETUP_JSON` with the new counts, `members`, and a third `sign_in_methods` option; assert in `the_setup_outline_reads_as_the_server_sends_it` that `members[1].passkey == false`, `counts.owner_has_provider == true`; in `a_setup_outline_with_odd_parts_still_reads…` that a `members` row missing `display_name` is dropped and a JSON without `members` reads as empty. Add `a_refusal_about_the_provider_reads_and_an_unknown_one_does_not` for the two new `why` values. Add `a_passkey_list_reads_with_nulls_where_a_key_was_never_used`.

- [ ] **Step 2: Run**: `cargo test -p protocol tiny` → FAIL.

- [ ] **Step 3: Implement** per Interfaces.

- [ ] **Step 4: Run**: `cargo test -p protocol` → PASS.

- [ ] **Step 5: Commit**

```bash
git add docs/superpowers crates/protocol
git commit -m "Protocol: three sign-in methods, who is ready, passkey links and a member's own passkeys"
```

---

### Task 2: chatd — the calls

**Files:**
- Modify: `crates/chatd/src/adapters/tiny/manage.rs:535-660` (`setup_bounded` truncates `members` to `SETUP_ROWS_MAX`·8 and clips `display_name`; `setup_refusal` two new reasons; new `setup_claim_links`, `passkeys_read`, `passkey_rename`, `passkey_remove`, `recovery_codes`)
- Modify: `crates/chatd/src/server.rs:2153-2155` (dispatch the five requests)
- Test: `crates/chatd/src/adapters/tiny/manage/tests.rs`

**Interfaces:**
- Consumes: Task 1's requests/responses.
- Produces: `pub fn setup_claim_links(account, user_ids: Vec<String>) -> Response` — `POST /v1/setup/claim-links` `{"user_ids": […]}` (no `days`, server default); 200 → `TinyClaimLinks`; 404 → `refused(OLDER)`; 400 → `refused(CHECK)`; owner-only via `owner_only` like `setup_read`. `passkeys_read` — `GET /v1/me/passkeys`; 404 → `OLDER`. `passkey_rename(account, id, label)` — `PATCH /v1/me/passkeys/{id} {"label"}`; label trimmed, 1–60 chars else `refused(CHECK)` without calling. `passkey_remove(account, id)` — `DELETE`; 403 `recent_sign_in_required` → `SIGN_IN_AGAIN`; 403 otherwise → `refused(LAST_PASSKEY)` with `pub const LAST_PASSKEY: &str = "This is your last passkey and you have no recovery codes left, so it stays."`. `recovery_codes(account)` — `POST /v1/me/recovery-codes`; 403 → `SIGN_IN_AGAIN`.

- [ ] **Step 1: Write the failing tests** on the fake server at tests.rs:331: `the_owner_gets_passkey_links_and_an_older_server_says_so`, `a_members_own_passkeys_are_read_renamed_and_removed`, `removing_the_last_passkey_without_codes_is_said_in_words`, `a_refusal_about_the_provider_reaches_the_gui_as_a_refusal` (409 `owner_needs_provider` → `TinySetupRefused`).

- [ ] **Step 2: Run**: `cargo test -p chatd --no-default-features manage` → FAIL.

- [ ] **Step 3: Implement.** Dispatch in `server.rs` next to the three existing lines.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/chatd
git commit -m "chatd: passkey links for the owner and a member's own passkeys"
```

---

### Task 3: `tiny_setup.rs` — the checklist model

**Files:**
- Modify: `crates/tinyclient/src/tiny_setup.rs` (`blocked`, `hint`, `remedy*`, `refusal_words`, `counts_words`, `sign_in_words`, new `Checklist`)
- Test: same file

**Interfaces (produces):**
```rust
pub enum StepState { Done, Open }
pub struct Step { pub words: String, pub state: StepState, pub action: Option<Action> }
pub enum Action { AddPasskey, SendLinks, SignInWithProvider }
pub struct Checklist { pub steps: Vec<Step>, pub anyway: bool }  // anyway: Apply must be "Switch anyway" (Danger)
pub fn checklist(setup: &TinySetup, form: &Form) -> Option<Checklist>
```
- `None` unless `form.picked.sign_in_methods != setup.current.sign_in_methods` and the choice is relevant.
- Target `passkey`: step 1 "You have a passkey" (Done if `counts.owner_has_passkey`, else Open with `AddPasskey`); step 2 "{n} of {m} members have a passkey" where m = `members.len()` or `counts.members` when `members` is empty, n = m − `members_without_passkey` (Done when `members_without_passkey == 0`; Open with `SendLinks` only when `members` lists at least one with `passkey == false` — an older server with no list gets no action, Review Focus 5). `anyway = members_without_passkey > 0`.
- Target `provider`: step 1 "You can sign in through {provider}" (Done if `counts.owner_has_provider`, else Open with `SignInWithProvider`); step 2 "{n} of {m} members can"; `anyway = members_without_provider > 0`. No link action (the client cannot make a provider account for someone).
- Target `any`: `Some(Checklist { steps: [], anyway: false })` — nothing in the way; the GUI draws nothing but Apply stays Primary.
- `remedy(refusal)`: `OwnerNeedsProvider` → `None` (the client cannot drive a provider sign-in for someone already signed in); `refusal_words` for it: "You haven't signed in through {provider} yourself yet, so you would be locked out. Sign in through it once, then try again." `MembersWithoutProvider { count }` → `DoItAnyway`, words "{count} members have no {provider} account and couldn't sign in until they get one."
- `sign_in_words` gains `provider`. `counts_words` says both counts. `ADD_PASSKEY_WORDS` unchanged.
- `Form::pick_option` no longer refuses on `needs` — the checklist replaces blocking; `blocked()` stays only for `"passkeys"` against an older server.

- [ ] **Step 1: Write the failing tests**: `a_switch_to_passkeys_lists_the_two_steps_and_who_can_act`, `a_switch_back_to_the_provider_mirrors_them`, `no_switch_or_the_middle_way_needs_no_steps`, `an_older_server_with_no_member_list_still_counts_but_offers_no_links`, `switch_anyway_is_asked_only_when_someone_would_be_locked_out`, and update `a_preset_or_option_the_server_cannot_do_is_not_picked` (Privacy is picked now; only the older-server `passkeys` need still blocks) and `every_value_the_server_can_send_has_words_for_a_member` (three values). Extend the `setup()` fixture with `members` (owner, two without passkey, one without provider) and the new counts.

- [ ] **Step 2: Run**: `cargo test -p tinyclient tiny_setup` → FAIL.

- [ ] **Step 3: Implement.**

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/tinyclient/src/tiny_setup.rs
git commit -m "The sign-in choice is a checklist the owner can act on, not a refusal"
```

---

### Task 4: The two setup screens draw the checklist and hand out links

**Files:**
- Modify: `crates/tinyclient/src/gui/tiny/setup.rs:419-560` (`tiny_setup_form`, refusal), `:573-640` (overlay footer), `:146-380` (new actions)
- Modify: `crates/tinyclient/src/gui/tiny/mod.rs:79-106` (`Modal::PasskeyLinks { picked: Vec<String>, links: Vec<TinyClaimLink>, busy: bool, error: Option<String> }`), state for it
- Modify: `crates/tinyclient/src/gui/tiny/modals.rs` (render it)
- Modify: `crates/tinyclient/src/gui/tiny/settings/privacy.rs` only if the Apply label/kind is decided there (it is in setup.rs's `unsaved_bar` hook; check `tiny_privacy_save`)

**Interfaces:**
- Consumes: Task 3's `checklist`; Task 2's `TinySetupClaimLinks`.
- Produces: `tiny_setup_checklist(&self, setup, cx) -> Option<Div>` drawn directly under the picked option's `pick_card` in the `sign_in_methods` block; `tiny_setup_open_links(window, cx)` opens the modal with every `members[].passkey == false` pre-ticked; `tiny_setup_make_links(cx)` sends the request; the modal lists `display_name` — `Copy` per row and `Copy all` (one `name<TAB>url` line per member) via `cx.write_to_clipboard(ClipboardItem::new_string(..))`; closing discards.

- [ ] **Step 1: The sign-in block.** Three radio rows as today (the server sends three options). No `blocked` warning text unless the older-server reason. Under the picked row, when `checklist()` is `Some` with steps: a `card()` with one line per step — `glyph("icons/check.svg", theme::positive())` for Done, `glyph("icons/circle.svg", theme::warning())` for Open — the words, and at the right the action button: `AddPasskey` → "Add a passkey for me" (existing `tiny_setup_link`), `SendLinks` → "Send passkey links…" (`tiny_setup_open_links`), `SignInWithProvider` → no button, the words carry it.
- [ ] **Step 2: Apply.** When `checklist().map_or(false, |c| c.anyway)`: label "Switch anyway", `ButtonKind::Danger`, and the form sets `allow_lockout = true` before sending (so the server's `members_without_*` refusal is not hit twice). Otherwise as today. Same on the Privacy page's unsaved bar: find where `tiny_privacy_save` builds the label and route through the same helper `fn apply_button(&self, setup) -> (label, ButtonKind)` in setup.rs.
- [ ] **Step 3: Preset cards.** Delete the `unavailable` amber paragraph path only when `available` is true (the server now always says true); keep the branch for older servers but shorten it to one line in `theme::warning()`.
- [ ] **Step 4: The modal.** `Modal::PasskeyLinks`. Before links exist: the ticked member list (`check_box` rows), "Make links" Primary. After: each row `display_name`, the URL in `type_small()` faint, `Copy`; footer "Copy all" Secondary and "Done" Ghost; a note: "Each link works once, for that person only, for 14 days. Send it to them yourself; the server does not." On `OLDER`/error: `note("icons/triangle-alert.svg", theme::warning(), words)` in place of the list (Review Focus 3).
- [ ] **Step 5: Refusals.** `tiny_setup_refusal` handles the two new variants through Task 3's `remedy`/`refusal_words`.
- [ ] **Step 6: Compile** `cargo check -p tinyclient --features gui` → clean. **Shots**: replace the `nopasskeys` mode of `tiny_shot_setup` with `checklist` (Privacy picked, owner has no passkey, 3 of 8 members without; customizing open) and add `links` (the modal open with two links made). Update `SHOT_OUTLINE` with `members` and the new counts. Take `--demo --mode tiny --tiny-setup first|checklist|links|privacy --shot` and look at each.
- [ ] **Step 7: Commit**

```bash
git add crates/tinyclient/src/gui/tiny
git commit -m "Setup draws the steps under the sign-in choice and hands out passkey links"
```

---

### Task 5: Server Settings › Sign-in — your passkeys

**Files:**
- Modify: `crates/tinyclient/src/gui/tiny/settings/signin.rs` (new "Passkeys" section between Provider and Sessions)
- Modify: `crates/tinyclient/src/gui/tiny/mod.rs` (`Modal::RecoveryCodes(Vec<String>)`, `Modal::RemovePasskey { id, label }`, state `passkeys: Option<TinyPasskeys>`, `passkeys_error`, `renaming: Option<(String, Entity<TextInput>)>`)
- Modify: `crates/tinyclient/src/gui/tiny/modals.rs` (render both)
- Modify: `crates/tinyclient/src/gui/tiny/settings/mod.rs:337` (on entering `Page::SignIn`, send `TinyPasskeysRead`)

**Interfaces:**
- Consumes: Task 2's four passkey calls and `TinyPasskeyLink` (existing).
- Produces: section "Passkeys": `card()` with a row per `TinyPasskey`: label (or the rename `input` when `renaming` matches), "Made {date} · Last used {date|never}", a "Synced" chip when `synced`, a "Flagged" chip in `theme::warning()` when `flagged`; row actions `Rename` (Ghost) and `Remove` (Ghost, opens `Modal::RemovePasskey`, disabled with tip `LAST_PASSKEY` when it is the only passkey and `recovery_codes_left == 0`, Review Focus 4). Under the card: "Add a passkey" (Primary; existing `tiny_setup_link` logic, moved to a shared `tiny_passkey_link`), "New recovery codes" (Secondary → `TinyRecoveryCodes` → `Modal::RecoveryCodes`), and the line "{n} recovery codes left." `empty("icons/key.svg", "No passkeys yet. Add one to sign in without {provider}.")` when the list is empty; an older server's `OLDER` reply hides the whole section.
- `Modal::RecoveryCodes`: the ten codes in a monospace block, "Copy" and "Done", and the words "Each code works once. Keep them somewhere that is not this computer."

- [ ] **Step 1: Draw it**, following `signin.rs`'s existing `section`/`card`/`line` style and `invites.rs` for the row-with-actions pattern.
- [ ] **Step 2: Compile** `cargo check -p tinyclient --features gui` → clean. Shot: `--demo --mode tiny --tiny-settings signin --shot` with two demo passkeys seeded in the demo state (add to the demo's Tiny community where `--tiny-owner` seeds things).
- [ ] **Step 3: Commit**

```bash
git add crates/tinyclient/src/gui/tiny
git commit -m "Sign-in settings: see, add, rename and remove your passkeys; new recovery codes"
```

---

### Task 6: Docs, whole-branch check

- [ ] `docs/design/10-tiny-server.md:300-305`: the `--tiny-setup` list becomes `first|custom|refused|checklist|links|privacy|change|member`; a paragraph on the checklist and the Passkeys section.
- [ ] `cargo fmt --all --check && cargo clippy --workspace --all-targets --features gui -- -D warnings && cargo test -p protocol && cargo test -p tinyclient && cargo test -p chatd --no-default-features` → PASS.
- [ ] Push once: `git push -u origin feat/sign-in-switch`; PR body from the spec's "What this builds" items 5 and 6, with the four shots attached.
