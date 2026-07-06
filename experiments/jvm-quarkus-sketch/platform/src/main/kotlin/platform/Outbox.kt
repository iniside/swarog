package platform

import java.sql.ResultSet
import javax.sql.DataSource

/** A single transactional-outbox entry, drained by a per-module relay. */
data class OutboxRow(val id: Long, val topic: String, val payload: String)

/**
 * Shared drain plumbing for the transactional outbox. The relay ITSELF lives per-module — each owns
 * the mapping from its topics to subscriber URLs it POSTs to — but the "read unsent / mark sent" SQL
 * is identical across every owning schema and homes here (JDBC only, no feature or JPA knowledge).
 *
 * `schema` is always a module-owned literal (never external input), so interpolating it into the
 * statement is safe.
 */
object Outbox {

    /** Unsent rows for `schema.outbox`, oldest first — the relay's per-tick work list. */
    fun unsent(db: DataSource, schema: String): List<OutboxRow> =
        db.connection.use { c ->
            c.prepareStatement(
                "SELECT id, topic, payload FROM $schema.outbox WHERE sent_at IS NULL ORDER BY id"
            ).use { ps -> ps.executeQuery().use(::readRows) }
        }

    /** Row-mapping extracted to its own function — the JDBC try-with-resources chain above (three
     *  nested `.use{}`) plus an inline while-loop tripped detekt's NestedBlockDepth; the loop body
     *  belongs in its own function anyway. */
    private fun readRows(rs: ResultSet): List<OutboxRow> {
        val rows = ArrayList<OutboxRow>()
        while (rs.next()) rows.add(OutboxRow(rs.getLong(1), rs.getString(2), rs.getString(3)))
        return rows
    }

    /** Mark a row delivered. Called only AFTER a successful emit — a failed emit leaves it NULL
     *  so the next tick retries (at-least-once). */
    fun markSent(db: DataSource, schema: String, id: Long) {
        db.connection.use { c ->
            c.prepareStatement("UPDATE $schema.outbox SET sent_at = now() WHERE id = ?").use { ps ->
                ps.setLong(1, id)
                ps.executeUpdate()
            }
        }
    }
}
