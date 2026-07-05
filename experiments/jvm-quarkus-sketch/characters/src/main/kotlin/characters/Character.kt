package characters

import io.quarkus.hibernate.orm.panache.kotlin.PanacheCompanionBase
import io.quarkus.hibernate.orm.panache.kotlin.PanacheEntityBase
import jakarta.persistence.Column
import jakarta.persistence.Entity
import jakarta.persistence.GeneratedValue
import jakarta.persistence.GenerationType
import jakarta.persistence.Id
import jakarta.persistence.Table
import java.util.UUID

/**
 * `characters.characters` as an entity. The table's BIGSERIAL means IDENTITY generation here —
 * NOT the `PanacheEntity` base class, whose default is a Hibernate-named sequence that doesn't
 * match a SERIAL column. `playerId` is a PLAIN UUID column, deliberately NOT a `@ManyToOne
 * Player` — an association across module boundaries is exactly what the architecture forbids,
 * and the ORM is the first tool in this codebase that would happily let us violate it.
 */
@Entity
@Table(name = "characters", schema = "characters")
class Character(
    @Id
    @GeneratedValue(strategy = GenerationType.IDENTITY)
    var id: Long? = null,

    @Column(name = "player_id")
    var playerId: UUID = UUID(0, 0),

    @Column(name = "name")
    var name: String = "",
) : PanacheEntityBase {
    companion object : PanacheCompanionBase<Character, Long>
}
