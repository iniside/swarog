# RefPicker

A button that shows and changes the branch, tag or revision a page is looking at.

## When to use

- Above file trees, history and compare views, to pick the ref.
- Two pickers side by side for compare ("base", "compare").

## What the consumer provides

Plain markup: `<button class="refpicker"><span class="k">branch</span>main</button>` followed by a chevron icon. The `.k` label names the ref kind.

## Do and don't

- Do show the ref in full, in mono.
- Don't use it for a plain filter; use a `select`.
