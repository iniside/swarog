package domain

import io.quarkus.test.junit.QuarkusTestProfile

/**
 * A shared @TestProfile that turns the @Scheduled outbox relays OFF for a boot. The relays poll every
 * 1s and mark rows sent / fan out over HTTP; a test that drives a relay's `drain()` MANUALLY (or that
 * inspects a freshly-written outbox row's `sent_at`) must not race the background scheduler marking the
 * same row first. Disabling the scheduler makes those tests deterministic AND keeps `create()` from
 * triggering the real create -> outbox -> inventory-grant fan-out (no cross-schema side effects to clean).
 *
 * ONE profile class shared by every scheduler-sensitive test class, so Quarkus reuses a single boot for
 * all of them rather than rebooting per class.
 */
class SchedulerDisabledProfile : QuarkusTestProfile {
    override fun getConfigOverrides(): Map<String, String> = mapOf("quarkus.scheduler.enabled" to "false")
}
