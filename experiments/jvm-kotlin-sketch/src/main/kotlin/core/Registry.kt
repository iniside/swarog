package core

/**
 * Holds modules, orders them by their declared [Module.dependsOn] (topological sort),
 * and runs the lifecycle. Cycles and missing deps fail loudly at startup.
 *
 * Lifecycle:  init (wire) -> migrate (own schema) -> start (background).
 * Shutdown:   stop modules (reverse order) -> drain bus.
 */
class Registry(private val ctx: Context) {
    private val modules = LinkedHashMap<String, Module>()

    fun register(m: Module): Registry {
        require(modules.put(m.name, m) == null) { "duplicate module: ${m.name}" }
        return this
    }

    fun boot() {
        val ordered = topoSort()
        println("boot order: ${ordered.joinToString(" -> ") { it.name }}")
        for (m in ordered) m.init(ctx)
        for (m in ordered) if (m is Migrator) m.migrate(ctx)
        for (m in ordered) if (m is Starter) m.start(ctx)
    }

    fun shutdown() {
        for (m in topoSort().asReversed()) if (m is Stopper) m.stop()
        ctx.bus.drain()
    }

    /** Depth-first topo-sort. Detects cycles and missing dependencies, naming them. */
    private fun topoSort(): List<Module> {
        val ordered = ArrayList<Module>()
        val state = HashMap<String, Mark>()

        fun visit(name: String, stack: List<String>) {
            val m = modules[name]
                ?: error("missing dependency '$name' (required by '${stack.lastOrNull() ?: "?"}')")
            when (state[name]) {
                Mark.DONE -> return
                Mark.VISITING -> error(
                    "dependency cycle: " + (stack + name).dropWhile { it != name }.joinToString(" -> ")
                )
                null -> {}
            }
            state[name] = Mark.VISITING
            for (dep in m.dependsOn) visit(dep, stack + name)
            state[name] = Mark.DONE
            ordered.add(m)
        }

        for (name in modules.keys) visit(name, emptyList())
        return ordered
    }

    private enum class Mark { VISITING, DONE }
}
