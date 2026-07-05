package accounts

import accounts.accountsevents.PlayerRegistered
import accounts.accountsevents.PlayerRegisteredTopic
import core.Bus
import core.Context
import core.Migrator
import core.Module
import java.util.UUID
import javax.sql.DataSource

/** Owns player identity. Foundation module — depends on nothing but the core. */
class AccountsModule : Module, Migrator {
    override val name = "accounts"

    private lateinit var db: DataSource
    private lateinit var bus: Bus

    override fun init(ctx: Context) {
        db = ctx.db
        bus = ctx.bus
    }

    override fun migrate(ctx: Context) {
        db.connection.use { c ->
            c.createStatement().use { s ->
                s.execute("CREATE SCHEMA IF NOT EXISTS accounts")
                s.execute(
                    """CREATE TABLE IF NOT EXISTS accounts.players(
                        id         UUID PRIMARY KEY,
                        provider   TEXT NOT NULL,
                        created_at TIMESTAMPTZ NOT NULL DEFAULT now())"""
                )
            }
        }
    }

    /** dev-only self-registration (mirrors ACCOUNTS_DEV_AUTH). Emits PlayerRegistered. */
    fun register(provider: String): UUID {
        val id = UUID.randomUUID()
        db.connection.use { c ->
            c.prepareStatement("INSERT INTO accounts.players(id, provider) VALUES (?, ?)").use { ps ->
                ps.setObject(1, id)
                ps.setString(2, provider)
                ps.executeUpdate()
            }
        }
        bus.emit(PlayerRegisteredTopic, PlayerRegistered(id, provider))
        return id
    }
}
