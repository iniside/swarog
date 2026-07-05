// Cross-cutting infra shared by impl modules — no feature knowledge. Plain kotlin for now;
// later steps add Quarkus deps here for RoleConfig + the outbox row-model/mark-sent helper.
plugins {
    kotlin("jvm")
}
