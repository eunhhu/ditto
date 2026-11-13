# Task 044: The platform surface (profiles, instructions, skills, plugins)

The design, the reasons and the slice order are in
[design/platform-surface.md](../../design/platform-surface.md). This file
exists so that NEXT can link the task, and it records each slice's status in
one place. Slices P1 through P3 had been on main without a task file.

| Slice | Status | Decision | Evidence |
| --- | --- | --- | --- |
| P1 the profile boundary | done | ADR 0044 | `crates/profile` tests (`two_profiles_live_in_two_directories`), `ditto profile list\|path`, `ditto-daemon --profile` |
| P2 pinned standing instructions | pinned half done; the model-written half is not started | ADR 0045 | `read_only_turn` test `instructions::the_standing_instructions_lead_the_request_and_replay_keeps_the_old_text`, daemon `instructions` route test, payload 17 |
| P3 skills | done | ADR 0046 | `crates/profile::skill` tests, kernel `turn::skill` tests, `read_only_turn/skill.rs`, payloads 18 and 19 |
| P4 plugin manifests | library slice done | ADR 0048 | `crates/capability/src/install.rs` tests (9) |
| P5 provider conformance | not started | - | - |
| P6 native shell | not started | - | - |

## Live checks recorded when the slices landed

These checks were moved here from HANDOFF and were not re-run on 2026-11-13.

- **P1.** `DITTO_PROFILE=minchae DITTO_DATA_DIR=/tmp/dprof2` listened on 8899
  and wrote to `/tmp/dprof2/profiles/minchae`. `ditto profile list|path` read
  the data root, and `../etc` was refused by the name rule.
- **P2.** Against `127.0.0.1:8901`, `set` returned seq 2, `show` echoed the
  text, and `clear` returned `no standing instructions`.
- **P3.** A `photo-triage` skill was listed as one line plus its description,
  and `show` returned its body. `show ../etc` exited 1 with `it must start with
  a lowercase letter or digit`.

## Exit criteria still open

- **P2:** a model-written instruction change that lands on the next turn, with
  provenance, or a decision that the model does not edit instructions. The
  route's ingress rule is recorded in ADR 0045, "Who may change the standing
  instructions".
- **P4:** an event and a lease for install and uninstall, a daemon route, and
  the effect review of a package's declared effects before the package is
  exposed to a turn.
- **P3:** no live provider turn has used `skill.read`. The evidence so far is
  the scripted-model integration suite.
- **P5, P6:** as the design states.
