# Dependencies

Last updated: 2026-10-23

## In progress

_None._

## Open

### Known limits (push-routing-ordering-and-liveness step 9)

- [ ] **RustSec advisories in the dependency tree**
  The blocking `audit` stage of the Step 9 acceptance run
  `verify-1792616200-54429-317230a0` failed on 12 advisories published after the
  last green run: quinn-proto (5), rustls (4), rustls-webpki, webpki-roots,
  webpki-root-certs, event-listener (docs/roadmap/feature-tracker.md:319-321,
  2026-10-21). Fix is a dependency upgrade; every blocking `verifyctl` run stays red
  until then.
