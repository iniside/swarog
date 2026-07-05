package app

import accounts.AccountsModule
import characters.CharactersModule
import inventory.InventoryModule
import inventory.Owner
import inventory.OwnerType
import io.quarkus.runtime.StartupEvent
import jakarta.annotation.Priority
import jakarta.enterprise.context.ApplicationScoped
import jakarta.enterprise.event.Observes
import javax.sql.DataSource
import platform.RoleConfig

/**
 * Demo data so the dashboard isn't empty. Also re-runs the cross-module event-cleanup proof.
 *
 * vs the framework-free sketch: `app/Main.kt` — "the ONLY place that lists modules" — is gone.
 * CDI discovers the beans; nothing lists them. Which also means nothing SHOWS the wiring anymore:
 * the composition root was documentation, and this seed observer is all that's left of `app`.
 *
 * There is also no `bus.awaitIdle` — CDI gives no drain hook — so the seed POLLS the DB for the
 * async handlers' effects. Eventual consistency, made honest.
 *
 * Ordering: module migrations are order-independent BY CONSTRUCTION (own schema each, no
 * cross-module FKs), so they run at the default observer priority in whatever order the
 * container picks. The ONLY real ordering need in the whole app is "seed after all migrations"
 * — one late priority, knowing nothing about which modules exist.
 */
@ApplicationScoped
class Seed(
    private val db: DataSource,
    private val accounts: AccountsModule,
    private val characters: CharactersModule,
    private val inventory: InventoryModule,
    private val roleConfig: RoleConfig,
) {

    fun seed(@Observes @Priority(AFTER_ALL_MODULES) ev: StartupEvent) {
        if (!roleConfig.isActive("all")) return   // demo seed runs only in the full monolith
        // demo-only: start clean so the admin view is deterministic across runs
        db.connection.use { c ->
            // Also truncate the outbox/inbox so a rerun doesn't re-drive last run's events (dedup would
            // catch them, but the demo state should be pristine and deterministic).
            c.createStatement().use { s -> s.execute("TRUNCATE accounts.players, characters.characters, inventory.holdings, accounts.outbox, characters.outbox, inventory.inbox") }
        }

        val player = accounts.register("dev")
        val aragorn = characters.create(player, "Aragorn")
        characters.create(player, "Legolas")
        val gimli = characters.create(player, "Gimli")
        awaitUntil("starter swords granted") { holdingsCount() == 3 }

        inventory.add(Owner(OwnerType.CHARACTER, aragorn.toString()), "healing_potion", 3)

        characters.delete(gimli)  // event wipes Gimli's holdings — dashboard shows 2 chars, no orphans
        awaitUntil("deleted character's holdings wiped") {
            inventory.holdings(Owner(OwnerType.CHARACTER, gimli.toString())).isEmpty()
        }

        println("seeded: 3 characters created, 1 deleted (its holdings cleaned via event)")
        val gate = if (System.getenv("ADMIN_USER") != null) "(HTTP Basic on)"
                   else "(OPEN -- set ADMIN_USER/ADMIN_PASS to gate)"
        println("admin on http://localhost:8090/admin  $gate -- sections are LIVE. Ctrl+C to stop.")
    }

    private fun holdingsCount(): Int =
        db.connection.use { c ->
            c.createStatement().use { s ->
                s.executeQuery("SELECT count(*) FROM inventory.holdings").use { rs -> rs.next(); rs.getInt(1) }
            }
        }

    private fun awaitUntil(what: String, timeoutMs: Long = 2000, cond: () -> Boolean) {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (!cond()) {
            if (System.currentTimeMillis() > deadline) { System.err.println("seed: timed out waiting for $what"); return }
            Thread.sleep(10)
        }
    }

    companion object {
        /** Well past the default observer priority (2500), so every module's migration ran first. */
        private const val AFTER_ALL_MODULES = 5000
    }
}
