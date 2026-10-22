# Swarog Admin redesign mockups

Static HTML mockups for the redesigned admin portal. They exist to design the
panel contribution API, so every screen is assembled only from named panel
primitives contributed by modules (`assets/registry.js` holds the mock module
manifests).

Open `index.html` directly, or serve the folder:

```bash
python -m http.server 8765 --directory UILayout/redesign
```

Topbar controls (mockup only):

- **Show contributions** outlines every fragment with its module, extension
  point and primitive (`?annot=1`).
- **State** forces every contributed panel into loading / empty / error /
  stale / read-only / denied.
- Theme toggle (`?theme=light|dark`).

| Screen | File |
| --- | --- |
| Dashboard | `index.html` |
| Users & players / user detail | `players.html`, `player.html?u=aiko.tanaka&tab=lore` |
| Lore repositories / detail | `lore-repos.html`, `lore-repo.html?r=ashfall-game` |
| Lore permissions, locks, server, events, billing | `lore-*.html` |
| API keys (secret reveal: `?reveal=1`), mail, scheduler, config, audit, event plane | `apikeys.html`, `mail.html`, `scheduler.html`, `config.html`, `audit.html`, `events.html` |
| Economy | `economy.html` |
| **Primitive catalog** (API driver) | `catalog.html` |
| Panel states gallery | `states.html` |

Fake data lives in `assets/data.js` and is shared by every screen (deployment
`emberline-prod`, studio Emberline, game Ashfall).
