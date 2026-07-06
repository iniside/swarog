package domain

import characters.CharactersModule
import inventory.InventoryModule
import io.quarkus.runtime.StartupEvent
import io.quarkus.test.junit.QuarkusTest
import io.quarkus.test.junit.TestProfile
import jakarta.inject.Inject
import javax.sql.DataSource
import org.junit.jupiter.api.Assertions.assertDoesNotThrow
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

/**
 * P3-MIGRATION-IDEMPOTENCY + the cross-module-FK architectural rule, locked at the DB level.
 *
 *  - `migrate()` is `CREATE SCHEMA/TABLE … IF NOT EXISTS`, so re-running it (a second boot, a manual
 *    re-invoke) must be a no-op, not an error. We call the observer method twice on the live bean and
 *    assert it never throws. (Schema PRESENCE is deliberately NOT asserted — the shared DB is cumulative
 *    and CREATE-IF-NOT-EXISTS is unobservable; idempotency is the observable property.)
 *  - Cross-module FK absence: query `information_schema` for FOREIGN KEY constraints whose owning schema
 *    differs from the referenced table's schema, among the three module schemas. Per CLAUDE.md there must
 *    be ZERO — integrity across modules comes from events, not FK cascades. This is the ONE allowed
 *    schema-shape check (existence-of-FK, not absence-of-schema). Deliberate-break: adding e.g.
 *    `ALTER TABLE inventory.holdings ADD FOREIGN KEY (owner_id) REFERENCES characters.characters(id)`
 *    makes the count non-zero → RED.
 *
 * Scheduler off ([SchedulerDisabledProfile]); this test writes no rows (DDL only), so nothing to clean.
 */
@QuarkusTest
@TestProfile(SchedulerDisabledProfile::class)
class MigrationIdempotencyTest {

    @Inject
    lateinit var inventory: InventoryModule

    @Inject
    lateinit var characters: CharactersModule

    @Inject
    lateinit var db: DataSource

    @Test
    fun `calling migrate twice is idempotent and does not throw`() {
        assertDoesNotThrow {
            inventory.migrate(StartupEvent())
            inventory.migrate(StartupEvent())
            characters.migrate(StartupEvent())
            characters.migrate(StartupEvent())
        }
    }

    @Test
    fun `no foreign key crosses a module schema boundary`() {
        val crossModuleFks = db.connection.use { c ->
            c.prepareStatement(CROSS_MODULE_FK_COUNT).use { ps ->
                ps.executeQuery().use { rs -> rs.next(); rs.getInt(1) }
            }
        }
        assertEquals(0, crossModuleFks, "no cross-module FK may exist — integrity crosses modules via events, not cascades")
    }

    private companion object {
        /**
         * Count FK constraints where the constraint's schema and the referenced (unique) table's schema are
         * two DIFFERENT module schemas. `constraint_column_usage` on the referential constraint's unique key
         * yields the referenced table; comparing its schema to the FK's schema surfaces any cross-module FK.
         */
        private const val CROSS_MODULE_FK_COUNT = """
            SELECT count(*)
            FROM information_schema.table_constraints tc
            JOIN information_schema.referential_constraints rc
              ON tc.constraint_schema = rc.constraint_schema AND tc.constraint_name = rc.constraint_name
            JOIN information_schema.constraint_column_usage ccu
              ON rc.unique_constraint_schema = ccu.constraint_schema AND rc.unique_constraint_name = ccu.constraint_name
            WHERE tc.constraint_type = 'FOREIGN KEY'
              AND tc.table_schema IN ('accounts', 'characters', 'inventory')
              AND ccu.table_schema IN ('accounts', 'characters', 'inventory')
              AND tc.table_schema <> ccu.table_schema
        """
    }
}
