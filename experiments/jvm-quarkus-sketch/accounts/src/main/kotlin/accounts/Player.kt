package accounts

import io.quarkus.hibernate.orm.panache.kotlin.PanacheCompanionBase
import io.quarkus.hibernate.orm.panache.kotlin.PanacheEntityBase
import jakarta.persistence.Column
import jakarta.persistence.Entity
import jakarta.persistence.Id
import jakarta.persistence.Table
import java.time.OffsetDateTime
import java.util.UUID

/**
 * `accounts.players` as an entity. The id is app-assigned (random UUID), not generated.
 * `created_at` keeps its DB-side DEFAULT now(): insertable=false makes Hibernate omit the
 * column on INSERT so the database fills it — the entity only ever reads it.
 */
@Entity
@Table(name = "players", schema = "accounts")
class Player(
    @Id
    var id: UUID = UUID(0, 0),

    @Column(name = "provider")
    var provider: String = "",

    @Column(name = "created_at", insertable = false, updatable = false)
    var createdAt: OffsetDateTime? = null,
) : PanacheEntityBase {
    companion object : PanacheCompanionBase<Player, UUID>
}
