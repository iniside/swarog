package core

/**
 * Typed key for the multi-value contribution registry ([Context.contribute] / [Context.contributions]).
 * Unlike the single-value service registry ([Context.provide] / require), MANY modules contribute to
 * one slot and a single consumer reads them all — e.g. admin sections. The analogue of Go's
 * `Context.Contribute(slot, v)` / `Contributions(slot)`.
 */
data class Slot<T>(val name: String)
