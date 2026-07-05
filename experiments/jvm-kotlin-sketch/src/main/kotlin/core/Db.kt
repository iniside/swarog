package core

import org.postgresql.ds.PGSimpleDataSource
import javax.sql.DataSource

/** One shared Postgres. `ctx.DB` is offered, not mandated — a module may bring its own store. */
object Db {
    fun fromEnv(): DataSource {
        // A DEDICATED database, isolated from the Go project's `gamebackend` DB (same schema/table
        // names would otherwise collide). Create once: CREATE DATABASE jvmsketch OWNER gamebackend;
        val url = System.getenv("DATABASE_URL")
            ?: "jdbc:postgresql://localhost:5432/jvmsketch?user=gamebackend&password=gamebackend&sslmode=disable"
        return PGSimpleDataSource().apply { setUrl(if (url.startsWith("jdbc:")) url else "jdbc:$url") }
    }
}
