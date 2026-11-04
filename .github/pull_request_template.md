## What changed and why

<!-- Two or three short sentences. What can a person do now that they could not, or what no longer breaks? -->

## Type

<!-- Check one. feature and fix need an e2e scenario below. -->

- [ ] feature
- [ ] fix
- [ ] perf
- [ ] refactor
- [ ] chore
- [ ] docs

## Platforms

<!-- Check the ones you know this reaches. The gate adds every platform it finds in the diff, and you cannot take one off. -->

- [ ] windows
- [ ] macos
- [ ] linux
- [ ] android
- [ ] ios

## Proof

<!-- feature and fix: the scenario that drives the real app and shows the change. It is in this diff.
     The lab records it on every platform above, once on the base (it fails) and once on this branch
     (it passes), and posts the `e2e-proof` status. Do not paste your own screenshots in its place. -->

Scenario: e2e/scenarios/<id>

Lab run: <!-- link the lab posts under `e2e-proof` -->

## Compatibility

<!-- Does an older server or an older client still work with this? If you changed the protocol, say which field and that it only grew. -->

## Risk and rollback

<!-- What breaks if this is wrong, and how it is undone: revert, a setting, a server deploy. -->

## Release note

<!-- One line, in plain words. It goes into the release summary. -->
