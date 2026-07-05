package characters

import admin.adminapi.Cell
import admin.adminapi.Item
import admin.adminapi.Kpi
import admin.adminapi.SectionData
import admin.adminapi.Table
import characters.charactersapi.PlayerCharacters
import characters.charactersevents.CharacterCreated
import characters.charactersevents.CharacterDeleted
import io.quarkus.narayana.jta.QuarkusTransaction
import io.quarkus.panache.common.Page
import io.quarkus.panache.common.Sort
import io.quarkus.runtime.StartupEvent
import jakarta.enterprise.context.ApplicationScoped
import jakarta.enterprise.event.Event
import jakarta.enterprise.event.Observes
import jakarta.enterprise.inject.Produces
import java.util.UUID
import javax.sql.DataSource
import platform.RoleConfig

/**
 * A player has N characters. `player_id` is a PLAIN column — no cross-module FK to accounts.
 * Provides [PlayerCharacters] by implementing it (CDI resolves by type — no provide/require),
 * and emits Created/Deleted (async). Has no idea `inventory` exists.
 */
@ApplicationScoped
class CharactersModule(
    private val db: DataSource,
    private val created: Event<CharacterCreated>,
    private val deleted: Event<CharacterDeleted>,
    private val roleConfig: RoleConfig,
) : PlayerCharacters {

    /** Default priority — order-independent by construction (own schema, no cross-module FKs). */
    fun migrate(@Observes ev: StartupEvent) {
        if (!roleConfig.isActive("characters")) return
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
        println("[characters] schema ready")
    }

    /** The contribution seam, now spelled CDI: producing an Item bean IS `contribute(AdminSection, …)`.
     *  No ordering here — `@All` discovery order is the container's business; the ADMIN sorts,
     *  because presentation order is the renderer's concern, not the contributors'. */
    @Produces
    @ApplicationScoped
    fun charactersAdminItem(): Item = Item(section = "Game Content", label = "Characters") {
        SectionData(
            kpis = listOf(Kpi("Characters", Character.count().toString())),
            table = Table(
                headers = listOf("ID", "Player", "Name"),
                rows = recent(10).map { ch ->
                    listOf(Cell(ch.id.toString(), mono = true), Cell(ch.playerId.toString(), mono = true), Cell(ch.name))
                },
            ),
        )
    }

    /** Write committed first, event fired after — see AccountsModule.register for why programmatic tx. */
    fun create(playerId: UUID, name: String): Long {
        val ch = Character(playerId = playerId, name = name)
        QuarkusTransaction.requiringNew().run { ch.persist() }
        val id = ch.id!!   // IDENTITY id assigned by the INSERT during the transaction
        created.fireAsync(CharacterCreated(id, playerId, name))
            .whenComplete { _, e -> if (e != null) System.err.println("event handler failed for CharacterCreated: $e") }
        return id
    }

    fun delete(id: Long) {
        val playerId = ownerOf(id) ?: return
        QuarkusTransaction.requiringNew().run { Character.deleteById(id) }
        // Integrity across modules comes from THIS event, not an FK cascade.
        deleted.fireAsync(CharacterDeleted(id, playerId))
            .whenComplete { _, e -> if (e != null) System.err.println("event handler failed for CharacterDeleted: $e") }
    }

    override fun ownerOf(characterId: Long): UUID? = Character.findById(characterId)?.playerId

    private fun recent(limit: Int): List<Character> =
        Character.findAll(Sort.descending("id")).page(Page.ofSize(limit)).list()
}
