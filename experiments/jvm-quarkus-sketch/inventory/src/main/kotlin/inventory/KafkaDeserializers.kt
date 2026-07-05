package inventory

import characters.charactersevents.CharacterCreated
import characters.charactersevents.CharacterDeleted
import io.quarkus.kafka.client.serialization.ObjectMapperDeserializer

/**
 * Kafka value deserializers for the character events inventory consumes in the SPLIT
 * topology (`%inventory` profile). [ObjectMapperDeserializer] needs a CONCRETE target
 * type, so each event gets a trivial typed subclass; their FQCNs
 * (`inventory.CharacterCreatedDeserializer` / `inventory.CharacterDeletedDeserializer`)
 * are named in `application.properties` under `.value.deserializer`.
 *
 * The producer side (`%characters`) uses the generic
 * `io.quarkus.kafka.client.serialization.ObjectMapperSerializer` — serialization needs
 * no type, deserialization does, hence subclasses here but none there.
 *
 * In the monolith these are never touched: the channel is internal and hands the object
 * over in-JVM without a wire (de)serialization step.
 */
class CharacterCreatedDeserializer :
    ObjectMapperDeserializer<CharacterCreated>(CharacterCreated::class.java)

class CharacterDeletedDeserializer :
    ObjectMapperDeserializer<CharacterDeleted>(CharacterDeleted::class.java)
