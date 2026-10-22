# WorkQueue

Items moving through states with attempt history: state counts act as tabs, each row carries the actions its state allows.

## When to use

Mail outbox, delivery queues, any retrying job list. The poison-event resolver on the event plane is the same idea for a paused subscription.

## What the consumer provides

State counts `[key, label, n]`, rows with `id`, target, state, attempts (`.attempts` pips: `f` failed, `ok` succeeded), next attempt and last error. Allowed actions per state: pending → cancel; retrying → retry, park, cancel; failed → retry, cancel; parked → resume, cancel; sent → none.

## Do and don't

- Do show the last error verbatim in mono.
- Do confirm Cancel; Retry and Park need no confirmation.
