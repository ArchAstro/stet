# Stet

*Stet* is a proofreader's mark. Written in the margin beside a correction, with
a row of dots under the words in question, it means "let it stand": ignore the
change, keep what was there. It is the oldest way of rejecting a suggestion,
which is half of what this editor's review mode is for.

## The mark

- **Icon** (`icon.png`): a serif lowercase *s* over five vermilion dots, on a
  warm paper tile. The dots are the stet mark.
- **Wordmark** (`banner.jpg`, `wordmark-tile.jpg`): *stet* in a serif book
  face on ink, dotted beneath, followed by a text cursor.

Write the product name as **Stet** in prose and `stet` where it is the command.

## Colour

| Role | Light (`stet`) | Dark (`stet-ink`) |
|---|---|---|
| Paper / ground | `#fbf7f0` | `#12151f` |
| Ink / text | `#1b1e27` | `#f3eee4` |
| Vermilion (cursor, selection, the dots) | `#d9432f` | `#ef6a55` |

Vermilion is the proofreader's red pencil. It is the only accent; use it for
the one thing on screen that marks where you are.

The two built-in themes carry these colours into the editor and are the
default. The ArchDev catalog themes sit alongside them unchanged.

## How the art was made

The icon and wordmark were generated with image models through
`archdev agents run --image`, then the icon tile was cut to the macOS icon
shape and grid.
