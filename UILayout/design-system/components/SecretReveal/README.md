# SecretReveal

A secret shown exactly once after a submit, with Copy and an acknowledgement before the dialog can close.

## When to use

Creating or rotating an API key, any credential the portal must hand over once. It arrives in a SubmitOutcome; the portal never stores it and a reload loses it.

## What the consumer provides

`Swarog.secret({ value, title?, text?, extra? })`. Put "I stored the secret somewhere safe" as a checkbox that enables Done.

## Do and don't

- Do say what to do if it is lost (revoke, create a new one).
- Don't log, cache or put the value in a URL.
