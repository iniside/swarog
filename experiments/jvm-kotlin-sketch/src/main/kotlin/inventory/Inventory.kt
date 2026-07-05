package inventory

import admin.adminapi.AdminSection
import admin.adminapi.Cell
import admin.adminapi.Item
import admin.adminapi.Kpi
import admin.adminapi.SectionData
import admin.adminapi.Table
import characters.charactersapi.PlayerCharacters
import characters.charactersevents.CharacterCreatedTopic
import characters.charactersevents.CharacterDeletedTopic
import core.Context
import core.Migrator
import core.Module
import core.require
import javax.sql.DataSource

enum class OwnerType { PLAYER, CHARACTER }

/** Polymorphic owner. `id` is a plain ref — for a character it's the character id as text. */
data class Owner(val type: OwnerType, val id: String)

/**
 * Owner-scoped holdings. Depends on accounts + characters.
 *  - SYNC-asks `characters.ownerOf` to authorize a character's inventory (downward dependency).
 *  - REACTS to character events: grant a starter item on create, wipe holdings on delete.
 * `characters` has no idea this module exists.
 */
class InventoryModule : Module, Migrator {
    override val name = "inventory"
    override val dependsOn = listOf("accounts", "characters")

    private lateinit var db: DataSource
    private lateinit var characters: PlayerCharacters

    override fun init(ctx: Context) {
        db = ctx.db
        characters = ctx.require<PlayerCharacters>() // sync capability, resolved downward

        // Sideways reactions go through the bus — async, fire-and-forget.
        ctx.bus.on(CharacterCreatedTopic) { ev ->
            grant(Owner(OwnerType.CHARACTER, ev.characterId.toString()), "starter_sword", 1)
            println("  [inventory] granted starter_sword to character ${ev.characterId}")
        }
        ctx.bus.on(CharacterDeletedTopic) { ev ->
            val wiped = wipe(Owner(OwnerType.CHARACTER, ev.characterId.toString()))
            println("  [inventory] wiped $wiped holding(s) for deleted character ${ev.characterId}")
        }

        // Contribute a dashboard section to the same slot characters uses — admin merges both.
        ctx.contribute(AdminSection, Item(section = "Game Content", label = "Inventory") {
            SectionData(
                kpis = listOf(
                    Kpi("Holdings", count().toString()),
                    Kpi("Owners", distinctOwners().toString()),
                ),
                table = Table(
                    headers = listOf("Owner", "ID", "Item", "Qty"),
                    rows = recentRows(20).map { r ->
                        listOf(
                            Cell(r.ownerType.lowercase(), badge = true),
                            Cell(r.ownerId, mono = true),
                            Cell(r.item),
                            Cell(r.qty.toString(), mono = true),
                        )
                    },
                ),
            )
        })
    }

    override fun migrate(ctx: Context) {
        db.connection.use { c ->
            c.createStatement().use { s ->
                s.execute("CREATE SCHEMA IF NOT EXISTS inventory")
                s.execute(
                    """CREATE TABLE IF NOT EXISTS inventory.holdings(
                        owner_type TEXT NOT NULL,
                        owner_id   TEXT NOT NULL,   -- plain polymorphic ref; NO cross-module FK
                        item       TEXT NOT NULL,
                        qty        INT  NOT NULL,
                        PRIMARY KEY (owner_type, owner_id, item))"""
                )
            }
        }
    }

    /** Authorizes a character inventory by SYNC-asking the capability — not the package. */
    fun add(owner: Owner, item: String, qty: Int) {
        if (owner.type == OwnerType.CHARACTER && characters.ownerOf(owner.id.toLong()) == null) {
            error("no such character ${owner.id} — refusing inventory write")
        }
        grant(owner, item, qty)
    }

    fun holdings(owner: Owner): List<Pair<String, Int>> =
        db.connection.use { c ->
            c.prepareStatement(
                "SELECT item, qty FROM inventory.holdings WHERE owner_type = ? AND owner_id = ? ORDER BY item"
            ).use { ps ->
                ps.setString(1, owner.type.name)
                ps.setString(2, owner.id)
                ps.executeQuery().use { rs ->
                    buildList { while (rs.next()) add(rs.getString(1) to rs.getInt(2)) }
                }
            }
        }

    private fun grant(owner: Owner, item: String, qty: Int) {
        db.connection.use { c ->
            c.prepareStatement(
                """INSERT INTO inventory.holdings(owner_type, owner_id, item, qty) VALUES (?, ?, ?, ?)
                   ON CONFLICT (owner_type, owner_id, item)
                   DO UPDATE SET qty = inventory.holdings.qty + EXCLUDED.qty"""
            ).use { ps ->
                ps.setString(1, owner.type.name)
                ps.setString(2, owner.id)
                ps.setString(3, item)
                ps.setInt(4, qty)
                ps.executeUpdate()
            }
        }
    }

    private fun wipe(owner: Owner): Int =
        db.connection.use { c ->
            c.prepareStatement("DELETE FROM inventory.holdings WHERE owner_type = ? AND owner_id = ?").use { ps ->
                ps.setString(1, owner.type.name)
                ps.setString(2, owner.id)
                ps.executeUpdate()
            }
        }

    private data class Row(val ownerType: String, val ownerId: String, val item: String, val qty: Int)

    private fun count(): Long =
        db.connection.use { c ->
            c.createStatement().use { s ->
                s.executeQuery("SELECT count(*) FROM inventory.holdings").use { rs -> rs.next(); rs.getLong(1) }
            }
        }

    private fun distinctOwners(): Long =
        db.connection.use { c ->
            c.createStatement().use { s ->
                s.executeQuery("SELECT count(*) FROM (SELECT DISTINCT owner_type, owner_id FROM inventory.holdings) t")
                    .use { rs -> rs.next(); rs.getLong(1) }
            }
        }

    private fun recentRows(limit: Int): List<Row> =
        db.connection.use { c ->
            c.prepareStatement("SELECT owner_type, owner_id, item, qty FROM inventory.holdings ORDER BY owner_id, item LIMIT ?").use { ps ->
                ps.setInt(1, limit)
                ps.executeQuery().use { rs ->
                    buildList { while (rs.next()) add(Row(rs.getString(1), rs.getString(2), rs.getString(3), rs.getInt(4))) }
                }
            }
        }
}
