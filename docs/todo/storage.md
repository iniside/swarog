# Storage

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **Misleading 409 at the object cap for put(expected_version = 0)**
  STORAGE-09. At 10,000 objects a create-style `put` to an address the owner
  already holds returns the "maximum of N objects" 409 instead of the
  address-taken one; same status, wrong remedy. The cap check runs before the
  insert (`modules/storage/src/service.rs:265-272`, comment states it);
  positive-version updates stay available
  (docs/roadmap/feature-tracker.md:320, 2026-09-17; read at HEAD).
