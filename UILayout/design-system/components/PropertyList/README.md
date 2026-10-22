# PropertyList

Label and value pairs for one object, with values rendered as the same cells tables use.

## When to use

The profile of an entity, process facts, the Store panel, any "details" block. Two columns of labels and values; labels in `text-3`.

## What the consumer provides

An array of `[label, valueHtml]`. Values may be badges, tags, refs (`Swarog.ref(kind, id)`), mono text or links. Put an Edit button in the panel tools to open a Form.

## Do and don't

- Do keep labels to one or two words.
- Don't use it for more than about twelve rows; that is a table.
