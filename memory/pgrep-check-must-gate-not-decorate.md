# The rollout check must GATE the command, not decorate it

Violated 2026-09-05 (Step 7 push-hub, subagent): ran
`pgrep -x cargo; pgrep -x rustc; cargo run -p verifyctl -- --bless-public-api`
as ONE compound bash call. `pgrep` printed a live PID — and the `cargo` in the
same line had already been dispatched, so the check informed nobody and a second
Cargo command started beside another agent's `cargo test`. "One test rollout at
a time — MANDATORY" was violated by the SHAPE of the invocation, not by skipping
it.

Rule: `pgrep -x cargo; pgrep -x rustc` is its OWN bash call. Read the output,
THEN decide. Never `pgrep …; cargo …`, never `pgrep … && cargo …` (that inverts:
`&&` runs cargo only when a rollout IS live).

Second lesson from the same incident: a `--bless-*` run regenerates EVERY
baseline, so it sweeps in unrelated pre-existing drift (here: `adminapi::slug`,
`SubmitOutcome::notice`, `MAIL_PRUNE`, a whole new `mailevents.txt` from the mail
rollout). After any bless, `git diff` the baseline dir and restore every file the
current change did not cause — a blessed contract diff nobody decided is a
semantic change smuggled into someone else's commit.
