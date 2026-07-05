package app

import accounts.AccountsModule
import admin.AdminModule
import characters.CharactersModule
import core.Context
import core.Db
import core.Registry
import inventory.InventoryModule
import inventory.Owner
import inventory.OwnerType
import java.util.concurrent.CountDownLatch

/**
 * The ONLY place that lists modules — the analogue of Go's cmd/server/main.go.
 * Boots, seeds a little data, then serves the admin panel until Ctrl+C.
 */
fun main() {
    val ctx = Context(Db.fromEnv())

    val accounts = AccountsModule()
    val characters = CharactersModule()
    val inventory = InventoryModule()
    val admin = AdminModule()

    val registry = Registry(ctx)
        .register(inventory)   // registration order is deliberately "wrong"...
        .register(characters)
        .register(admin)
        .register(accounts)
    registry.boot()            // ...topo-sort fixes it; admin.start() runs after every init(), so it sees all contributions

    seed(ctx, accounts, characters, inventory)

    Runtime.getRuntime().addShutdownHook(Thread {
        println("\nshutting down…")
        registry.shutdown()
    })
    println("open the admin URL above — sections are LIVE (each refresh re-queries the DB). Ctrl+C to stop.")
    CountDownLatch(1).await()
}

/** Demo data so the dashboard isn't empty. Also re-runs the cross-module event-cleanup proof. */
private fun seed(ctx: Context, accounts: AccountsModule, characters: CharactersModule, inventory: InventoryModule) {
    // demo-only: start clean so the admin view is deterministic across runs
    ctx.db.connection.use { c ->
        c.createStatement().use { s -> s.execute("TRUNCATE accounts.players, characters.characters, inventory.holdings") }
    }

    val player = accounts.register("dev")
    val aragorn = characters.create(player, "Aragorn")
    characters.create(player, "Legolas")
    val gimli = characters.create(player, "Gimli")
    ctx.bus.awaitIdle(2000) // starter_sword granted to each via events

    inventory.add(Owner(OwnerType.CHARACTER, aragorn.toString()), "healing_potion", 3)

    characters.delete(gimli)  // event wipes Gimli's holdings — dashboard shows 2 chars, no orphans
    ctx.bus.awaitIdle(2000)

    println("seeded: 3 characters created, 1 deleted (its holdings cleaned via event)")
}
