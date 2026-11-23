# AssetCard

Binary asset views: a before/after comparison in a diff (`.adiff`) and the asset file page (`.asset` with preview, info and lock box).

## When to use

- `.adiff` inside a DiffView `.dfile` for a changed binary asset: thumbnails and key properties side by side.
- `.asset` on the file page of a binary asset; `.lockbox.mine` and `.lockbox.other` show who holds its lock; `.variant-label` captions a state.

## What the consumer provides

Plain markup. `.adiff` holds two columns, each with `.lbl`, `.thumb` (`.thumb.after` on the right) and `dl.props`. `.asset` holds `.asset-prev` (with `.kind` and `.cap` labels) and `.asset-info`.

## Do and don't

- Do show the properties that changed (size, dimensions, LODs).
- Don't render real previews in mockups; the gradient thumbs stand in.
