package characters

import admin.adminapi.AdminSection
import admin.adminapi.Cell
import admin.adminapi.Item
import admin.adminapi.Kpi
import admin.adminapi.SectionData
import admin.adminapi.Table
import characters.charactersapi.PlayerCharacters
import characters.charactersevents.CharacterCreated
import characters.charactersevents.CharacterCreatedTopic
import characters.charactersevents.CharacterDeleted
import characters.charactersevents.CharacterDeletedTopic
import core.Bus
import core.Context
import core.Migrator
import core.Module
import java.util.UUID
import javax.sql.DataSource

/**
 * A player has N characters. `player_id` is a PLAIN column — no cross-module FK to accounts.
 * Provides [PlayerCharacters] (sync) and emits Created/Deleted (async).
 * Has no idea `inventory` exists.
 */
class CharactersModule : Module, Migrator, PlayerCharacters {
    override val name = "characters"
    override val dependsOn = listOf("accounts")

    private lateinit var db: DataSource
    private lateinit var bus: Bus

    override fun init(ctx: Context) {
        db = ctx.db
        bus = ctx.bus
        ctx.provide(PlayerCharacters::class, this) // publish the sync capability

        // Contribute a dashboard section — admin renders it without importing this module.
        // render() runs per request, so the numbers are always live.
        ctx.contribute(AdminSection, Item(section = "Game Content", label = "Characters") {
            SectionData(
                kpis = listOf(Kpi("Characters", count().toString())),
                table = Table(
                    headers = listOf("ID", "Player", "Name"),
                    rows = recent(10).map { (id, pid, name) ->
                        listOf(Cell(id.toString(), mono = true), Cell(pid, mono = true), Cell(name))
                    },
                ),
            )
        })
    }

    override fun migrate(ctx: Context) {
        db.connection.use { c ->
            c.createStatement().use { s ->
                s.execute("CREATE SCHEMA IF NOT EXISTS characters")
                s.execute(
                    """CREATE TABLE IF NOT EXISTS characters.characters(
                        id        BIGSERIAL PRIMARY KEY,
                        player_id UUID NOT NULL,   -- plain ref to accounts' player; NO cross-module FK
                        name      TEXT NOT NULL)"""
                )
            }
        }
    }

    fun create(playerId: UUID, name: String): Long {
        val id = db.connection.use { c ->
            c.prepareStatement(
                "INSERT INTO characters.characters(player_id, name) VALUES (?, ?) RETURNING id"
            ).use { ps ->
                ps.setObject(1, playerId)
                ps.setString(2, name)
                ps.executeQuery().use { rs -> rs.next(); rs.getLong(1) }
            }
        }
        bus.emit(CharacterCreatedTopic, CharacterCreated(id, playerId, name))
        return id
    }

    fun delete(id: Long) {
        val playerId = ownerOf(id) ?: return
        db.connection.use { c ->
            c.prepareStatement("DELETE FROM characters.characters WHERE id = ?").use { ps ->
                ps.setLong(1, id)
                ps.executeUpdate()
            }
        }
        // Integrity across modules comes from THIS event, not an FK cascade.
        bus.emit(CharacterDeletedTopic, CharacterDeleted(id, playerId))
    }

    override fun ownerOf(characterId: Long): UUID? =
        db.connection.use { c ->
            c.prepareStatement("SELECT player_id FROM characters.characters WHERE id = ?").use { ps ->
                ps.setLong(1, characterId)
                ps.executeQuery().use { rs -> if (rs.next()) rs.getObject(1, UUID::class.java) else null }
            }
        }

    private fun count(): Long =
        db.connection.use { c ->
            c.createStatement().use { s ->
                s.executeQuery("SELECT count(*) FROM characters.characters").use { rs -> rs.next(); rs.getLong(1) }
            }
        }

    private fun recent(limit: Int): List<Triple<Long, String, String>> =
        db.connection.use { c ->
            c.prepareStatement("SELECT id, player_id, name FROM characters.characters ORDER BY id DESC LIMIT ?").use { ps ->
                ps.setInt(1, limit)
                ps.executeQuery().use { rs ->
                    buildList { while (rs.next()) add(Triple(rs.getLong(1), rs.getString(2), rs.getString(3))) }
                }
            }
        }
}
