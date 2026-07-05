package inventory

import io.quarkus.hibernate.orm.panache.kotlin.PanacheCompanionBase
import io.quarkus.hibernate.orm.panache.kotlin.PanacheEntityBase
import jakarta.persistence.Column
import jakarta.persistence.Embeddable
import jakarta.persistence.EmbeddedId
import jakarta.persistence.Entity
import jakarta.persistence.Table
import java.io.Serializable

/**
 * The `inventory.holdings` table as a Panache/Hibernate entity. Maps the SAME schema the raw-JDBC
 * version created — same DDL in [InventoryModule.migrate], `schema = "inventory"` per the
 * logical-isolation rule, and NO association to any other module's entity: `ownerId` stays a
 * plain string ref, exactly like the plain column in SQL. (An ORM will happily let you add a
 * `@ManyToOne` across module boundaries — the one temptation raw SQL never offered.)
 *
 * The composite primary key costs real ceremony: a separate @Embeddable id class, Serializable,
 * column annotations (JPA would otherwise map `ownerType` -> `ownertype`). Panache's sweet spot
 * is the surrogate-Long-id entity (`PanacheEntity`); this table shows the off-road price.
 */
@Embeddable
data class HoldingId(
    @Column(name = "owner_type") var ownerType: String = "",
    @Column(name = "owner_id") var ownerId: String = "",
    @Column(name = "item") var item: String = "",
) : Serializable

@Entity
@Table(name = "holdings", schema = "inventory")
class Holding(
    @EmbeddedId
    var id: HoldingId = HoldingId(),

    @Column(name = "qty")
    var qty: Int = 0,
) : PanacheEntityBase {
    companion object : PanacheCompanionBase<Holding, HoldingId>
}
