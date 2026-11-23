# RawRecord

The stored record of one entity as pretty-printed, highlighted JSON, with a Copy button.

## When to use

The "Overview" or "Raw" tab of an entity whose module exposes its record as-is: a character, a group. Use it when an operator needs the exact stored shape, not a curated view; put the curated facts in a PropertyList next to it.

## What the consumer provides

The record object itself (`Swarog.RawRecord(value, { maxHeight })`), exactly as the module's admin read returns it: ids in full, timestamps as stored, `null` for unset fields. Put `Swarog.rawCopyBtn()` in the panel tools; it copies the printed text. Keys are highlighted in `accent`, strings in `ok`, numbers in `info`, booleans and `null` in `warn`.

## States

It renders inside a Panel, so loading, empty, error, stale, read-only and denied come from the Panel. Read-only changes nothing: the record is read-only already and Copy stays enabled.

## Do and don't

- Do pass the real stored record; don't add derived fields (counts, names joined from other modules).
- Don't use it for editing; edits go through a Form.
- Don't use it for logs or event payload streams; that is a Timeline.
